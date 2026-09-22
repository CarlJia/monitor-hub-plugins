//! tg-notify —— Telegram 通知示例插件，monitor-hub wasm 插件 ABI v2 的
//! 参考实现。ABI 的权威定义见仓库 `src/plugin/` 顶部的模块文档。
//!
//! 一个插件要做的全部事情：
//!
//! 1. 从 `"host"` 模块导入宿主函数（log / now / kv_get / kv_set /
//!    resp_alloc / http_post；v2 另有 http_get / nodes_query /
//!    emit_event / data_put / data_get / data_delete / data_list）；
//! 2. 导出 `memory`、`__alloc`、`on_event`（声明 tick/page/cleanup 时另有
//!    对应导出——本插件都不声明）；
//! 3. 在 `on_event(ptr, len)` 里解析事件 JSON，读自己的 kv 配置，发一次
//!    https POST，返回 0 表示成功。
//!
//! 本插件订阅宿主的离线/在线事件、面板登录成功/失败事件,以及财务插件发出的
//! `plugin_expiry_soon`,把通知渲染成中文文案发到 Telegram 的 `sendMessage`。
//! 渠道配置（bot_token / chat_id）由面板的 kv 编辑器写入，运行时经 `host_kv_get`
//! 读取——插件的 kv 命名空间是 `plugin.<plugin_id>:<key>`，`<plugin_id>` 取自
//! plugin.toml，key 里只要写 `bot_token` / `chat_id`。
//!
//! 文案本身也可配置：每类事件各有一份模板（`template_expiry_soon` /
//! `template_agent_offline` / `template_agent_online` / `template_login_succeeded`
//! / `template_login_failed`），用 `{字段}` 占位符插值。
//! 没配模板的那类走代码里的内置文案——与模板上线之前发出的内容逐字相同，所以
//! 升级不会改变已有部署看到的消息。模板按 Telegram 的 HTML 富样式写
//! （`parse_mode: "HTML"`）；插值进去的字段值由插件转义，认不出的占位符原样
//! 保留并打一条 warn——模板写错一个名字不该让整条通知消失。
//!
//! # on_event 的返回码
//!
//! | 码 | 含义 |
//! |----|------|
//! | 0  | 成功（sendMessage 返回 2xx） |
//! | 1  | 事件 JSON 解析失败 |
//! | 2  | kv 里没有 `bot_token` |
//! | 3  | kv 里没有 `chat_id` |
//! | 11–15 | http 失败：10 减去宿主错误码（11=参数越界，12=非 https，13=非 POST，14=网络失败，15=非 2xx） |

use std::ptr;

use chrono::DateTime;
use serde::Deserialize;
use serde_json::json;

// ---------------------------------------------------------------------------
// 宿主函数（R8）：全部从名为 "host" 的 wasm import 模块导入。
// 签名与错误码见 src/plugin.rs 的 host_linker。
// ---------------------------------------------------------------------------

#[link(wasm_import_module = "host")]
extern "C" {
    /// level：0=debug 1=info 2=warn 3=error。
    #[link_name = "log"]
    fn host_log(level: i32, ptr: i32, len: i32);
    /// 当前 Unix 秒。wasm 里没有时钟，时间一律向宿主要。
    #[link_name = "now"]
    fn host_now() -> i64;
    /// 发一个 `Content-Type: application/json` 的 https POST。返回写入 resp
    /// 缓冲的字节数；负数是错误码（-1 越界 -2 非 https -3 非 POST -4 网络
    /// -5 非 2xx）。
    #[link_name = "http_post"]
    fn host_http_post(
        method_ptr: i32,
        method_len: i32,
        url_ptr: i32,
        url_len: i32,
        body_ptr: i32,
        body_len: i32,
        resp_ptr: i32,
        resp_cap: i32,
    ) -> i32;
    /// 读 kv：返回写入 out 的字节数，0 = 无值，-1 = 越界/非法 UTF-8。
    #[link_name = "kv_get"]
    fn host_kv_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32;
    /// 写 kv（值上限 8 KiB）：0 成功，-1 越界/超限，-2 写库失败。本插件只读
    /// 配置不写状态，声明保留是为了展示完整的宿主函数面。
    #[allow(dead_code)]
    #[link_name = "kv_set"]
    fn host_kv_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> i32;
    /// 让宿主经 `__alloc` 分配响应缓冲并记住指针。本插件自己 `__alloc` 后把
    /// 指针直接传给 http_post，效果相同，因此声明但未使用——保留声明是为了
    /// 展示完整的宿主函数面。
    #[allow(dead_code)]
    #[link_name = "resp_alloc"]
    fn host_resp_alloc(cap: i32) -> i32;
}

// ---------------------------------------------------------------------------
// 分配器与内存工具
// ---------------------------------------------------------------------------

/// 线性内存里的堆顶。起始 1024，避开 wasm 栈（栈从 0 向下长）。每次派发宿主
/// 都新建实例，bump 分配永不归还也不会泄漏到下一次调用。
static mut HEAP_NEXT: usize = 1024;

/// 宿主写事件载荷、`host_resp_alloc` 回程都走这里：分配 cap 字节，返回指针。
#[no_mangle]
pub extern "C" fn __alloc(cap: i32) -> i32 {
    if cap <= 0 {
        return 0;
    }
    unsafe {
        let p = HEAP_NEXT;
        // 4 字节对齐，避免未对齐访问的额外指令开销。
        let size = (cap as usize + 3) & !3;
        HEAP_NEXT += size;
        p as i32
    }
}

/// 把字节写进线性内存（经 `__alloc`），返回 (ptr, len)。分配失败返回 (0, 0)。
fn write_bytes(bytes: &[u8]) -> (i32, i32) {
    // 空串也分配 1 字节，保证返回非 0 指针——宿主对空载荷也认。
    let ptr = __alloc(bytes.len().max(1) as i32);
    if ptr <= 0 {
        return (0, 0);
    }
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    (ptr, bytes.len() as i32)
}

/// `write_bytes` 的字符串版本。
fn write_str(s: &str) -> (i32, i32) {
    write_bytes(s.as_bytes())
}

/// 从线性内存切出事件载荷。ptr/len 由宿主传入，负值或 0 指针视为坏载荷。
fn read_payload(ptr: i32, len: i32) -> Option<&'static [u8]> {
    if ptr <= 0 || len <= 0 {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

// ---------------------------------------------------------------------------
// 事件载荷
// ---------------------------------------------------------------------------

/// v2 的事件词表，与 hub 的 `notification_bus::Event` 序列化形状一致（按字段名
/// 反序列化，新增字段被自动忽略，旧插件不会因为词表扩展而坏掉）。v2 起宿主
/// 自身的到期检测退役，到期通知改由财务插件经 `emit_event` 发出，事件名带
/// `plugin_` 前缀——这里用 `rename` 把它映到带标签的变体上。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
enum Event {
    /// 财务插件发出的到期提醒（原宿主的 expiry_soon，字段未变）。
    #[serde(rename = "plugin_expiry_soon")]
    PluginExpirySoon {
        node_id: i64,
        name: String,
        expires_at: String,
        days_left: i64,
        threshold_days: i64,
    },
    AgentOffline {
        node_id: i64,
        name: String,
        observed_at: i64,
        last_seen_at: i64,
    },
    AgentOnline {
        node_id: i64,
        name: String,
        observed_at: i64,
    },
    /// 面板登录成功。`method` 是渠道(`password` / `github`),`actor` 是登录主体
    /// （GitHub 用户名；应急密码没有账号，宿主发空串），`ip` 是发起端地址。
    LoginSucceeded {
        method: String,
        actor: String,
        ip: String,
        observed_at: i64,
    },
    /// 面板登录失败。`reason` 是宿主给出的原因文案（密码错、GitHub 不在白名单、
    /// state 不匹配等），`ip` 是发起端地址。
    LoginFailed {
        method: String,
        reason: String,
        ip: String,
        observed_at: i64,
    },
}

// ---------------------------------------------------------------------------
// 模板与内置文案
// ---------------------------------------------------------------------------

/// 三类事件的模板 kv key。与 plugin.toml 的 `[[kv]] key` 逐字一致。
const TEMPLATE_EXPIRY_SOON: &str = "template_expiry_soon";
const TEMPLATE_AGENT_OFFLINE: &str = "template_agent_offline";
const TEMPLATE_AGENT_ONLINE: &str = "template_agent_online";
const TEMPLATE_LOGIN_SUCCEEDED: &str = "template_login_succeeded";
const TEMPLATE_LOGIN_FAILED: &str = "template_login_failed";

/// 没配模板时用的内置文案。**必须与 plugin.toml 里对应 `[[kv]]` 的 `default`
/// 逐字一致**——面板预填的就是那份,漂移了操作员看到的基准就不是插件真会发的
/// 那段话。`tests/smoke.rs` 的漂移守卫测试拿实际发出的文案与 manifest 比对。
const BUILTIN_EXPIRY_SOON: &str = "⏰ 节点 {name} 将于 {expires_at} 到期（剩 {days_left} 天）";
const BUILTIN_AGENT_OFFLINE: &str = "🔴 节点 {name} 已离线（最后上报于 {silent_for} 秒前）";
const BUILTIN_AGENT_ONLINE: &str = "🟢 节点 {name} 已恢复在线";
const BUILTIN_LOGIN_SUCCEEDED: &str = "✅ 面板登录成功（{method}）{actor}，来自 {ip}，时间 {observed_at}";
const BUILTIN_LOGIN_FAILED: &str = "⚠️ 面板登录失败（{method}），来自 {ip}，时间 {observed_at}；原因：{reason}";

/// kv 值的上限（宿主侧 `KV_VALUE_MAX`，8 KiB）。它不在 ABI 里，所以这里是手抄的
/// 常量：模板最长可以到这个量级，读的时候缓冲给小了会**静默截断**——发出去的
/// 消息少半句而没人报错，比读不到更难查。
const KV_BUF_CAP: i32 = 8 * 1024;

/// 时区偏移的 kv key。与 plugin.toml 的 `[[kv]] key` 逐字一致。
const TZ_OFFSET: &str = "tz_offset";

/// 读时区偏移（相对 UTC 的小时数，支持半小时如 5.5），换算成秒。没配、空白、
/// 非法或越界都回东八区 +8——与面板 `default` 预填的基准一致；非法值另外打一
/// 条 warn（空白不算：面板「清空保存」写进去的就是空串，那是有意的重置）。
/// 写错时区不该让通知消失。
fn tz_offset_secs() -> i32 {
    const DEFAULT_SECS: i32 = 8 * 3600;
    let Some(raw) = kv_get_string(TZ_OFFSET) else {
        return DEFAULT_SECS;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return DEFAULT_SECS;
    }
    match raw.parse::<f64>() {
        Ok(hours) if (-12.0..=14.0).contains(&hours) => (hours * 3600.0).round() as i32,
        _ => {
            log(
                2,
                &format!("tg-notify: tz_offset「{raw}」不是合法的时区偏移（-12 到 14 的小时数），按东八区处理"),
            );
            DEFAULT_SECS
        }
    }
}

/// Unix 秒 → 消息里的时间文案：按 tz_offset 换算后带时区标注显示，如
/// `2026-09-20 16:00 UTC+8`。wasm 里拿不到系统时区，偏移完全由配置决定（缺省
/// 东八区）。chrono 只做日期算术，时钟来自事件载荷本身。
fn fmt_ts(secs: i64, tz_secs: i32) -> String {
    DateTime::from_timestamp(secs + tz_secs as i64, 0)
        .map(|d| format!("{} {}", d.format("%Y-%m-%d %H:%M"), tz_label(tz_secs)))
        .unwrap_or_default()
}

/// 偏移秒数 → `UTC+8` / `UTC-5` / `UTC+5:30` 这样的标注。整点不带分钟。
fn tz_label(tz_secs: i32) -> String {
    let sign = if tz_secs < 0 { '-' } else { '+' };
    let abs = tz_secs.abs();
    let (h, m) = (abs / 3600, (abs % 3600) / 60);
    if m == 0 {
        format!("UTC{sign}{h}")
    } else {
        format!("UTC{sign}{h}:{m:02}")
    }
}

/// 一类事件的三件东西：模板的 kv key、内置文案、以及占位符取值。三样放在一处，
/// 免得日后加了字段忘了在模板里露出来。
fn material(event: &Event) -> (&'static str, &'static str, Vec<(&'static str, String)>) {
    // 时间类占位符的显示时区：每条事件读一次配置，没配即东八区。
    let tz = tz_offset_secs();
    match event {
        Event::PluginExpirySoon {
            node_id,
            name,
            expires_at,
            days_left,
            threshold_days,
        } => (
            TEMPLATE_EXPIRY_SOON,
            BUILTIN_EXPIRY_SOON,
            vec![
                ("node_id", node_id.to_string()),
                ("name", name.clone()),
                ("expires_at", expires_at.clone()),
                ("days_left", days_left.to_string()),
                ("threshold_days", threshold_days.to_string()),
            ],
        ),
        Event::AgentOffline {
            node_id,
            name,
            observed_at,
            last_seen_at,
        } => {
            // 静默时长向宿主要时钟算（wasm 里没有时钟）。负值取 0：时钟回拨时
            // 不该出现「-3 秒前」这种消息。
            let silent_for = (unsafe { host_now() } - last_seen_at).max(0);
            (
                TEMPLATE_AGENT_OFFLINE,
                BUILTIN_AGENT_OFFLINE,
                vec![
                    ("node_id", node_id.to_string()),
                    ("name", name.clone()),
                    ("observed_at", fmt_ts(*observed_at, tz)),
                    ("last_seen_at", fmt_ts(*last_seen_at, tz)),
                    ("silent_for", silent_for.to_string()),
                ],
            )
        }
        Event::AgentOnline {
            node_id,
            name,
            observed_at,
        } => (
            TEMPLATE_AGENT_ONLINE,
            BUILTIN_AGENT_ONLINE,
            vec![
                ("node_id", node_id.to_string()),
                ("name", name.clone()),
                ("observed_at", fmt_ts(*observed_at, tz)),
            ],
        ),
        Event::LoginSucceeded {
            method,
            actor,
            ip,
            observed_at,
        } => (
            TEMPLATE_LOGIN_SUCCEEDED,
            BUILTIN_LOGIN_SUCCEEDED,
            vec![
                ("method", method.clone()),
                ("actor", actor.clone()),
                ("ip", ip.clone()),
                ("observed_at", fmt_ts(*observed_at, tz)),
            ],
        ),
        Event::LoginFailed {
            method,
            reason,
            ip,
            observed_at,
        } => (
            TEMPLATE_LOGIN_FAILED,
            BUILTIN_LOGIN_FAILED,
            vec![
                ("method", method.clone()),
                ("reason", reason.clone()),
                ("ip", ip.clone()),
                ("observed_at", fmt_ts(*observed_at, tz)),
            ],
        ),
    }
}

/// 按事件类型渲染通知文案：配了模板就用模板，没配（无值或纯空白）用内置文案。
///
/// 插值进去的字段值做 HTML 转义——节点名是别人给的，一个 `<` 就能把消息结构改掉。
/// 认不出的占位符原样保留，并**汇总成一条** warn（不逐个占位符刷日志）。
fn render(event: &Event) -> String {
    let (key, builtin, vars) = material(event);
    let template = kv_get_string(key)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| builtin.to_owned());
    let vars: Vec<(&str, String)> = vars
        .into_iter()
        .map(|(k, v)| (k, escape_html(&v)))
        .collect();
    let (text, unknown) = substitute(&template, &vars);
    if !unknown.is_empty() {
        let known: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
        log(
            2,
            &format!(
                "tg-notify: 模板 {key} 里的占位符 {} 认不出，已原样保留；这条事件可用的是 {}",
                unknown.join("、"),
                known.join("、")
            ),
        );
    }
    text
}

/// 用 `{名字}` 占位符替换。认不出的原样留着——模板写错一个名字，消息还是发得出去。
///
/// 没有 `{` 的转义语法：认不出的占位符本来就原样保留，所以模板里写一个字面花括号
/// 不会卡住（代价是写不出一个字面的 `{name}`）。
fn substitute(template: &str, vars: &[(&str, String)]) -> (String, Vec<String>) {
    let mut out = String::with_capacity(template.len() + 64);
    let mut unknown = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match vars.iter().find(|(key, _)| *key == name) {
                    Some((_, value)) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                        unknown.push(name.to_owned());
                    }
                }
                rest = &after[close + 1..];
            }
            // 没收口的 `{`：原样留着，别把它之后整段都吞掉。
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, unknown)
}

/// 插值用的 HTML 转义。模板原文是作者按 HTML 写的、原样发出；值不是——节点名里
/// 的 `<`、`&` 都会破坏消息。`"` 也要转义：模板写在属性里（如
/// `<a href="…{name}…">`）时,值里的引号会提前闭合属性、整条消息被 Telegram 拒
/// 收——而「转义」的承诺让人以为这种情况已经被处理。只转这四个字符：Telegram 的
/// HTML 解析认它们(`&quot;` 是标准实体,会按字面 `"` 解析)。
fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// kv 配置
// ---------------------------------------------------------------------------

/// 读一个 kv 配置项。无值（返回 0）、越界（负数）或非法 UTF-8 都归成 None。
fn kv_get_string(key: &str) -> Option<String> {
    let (kptr, klen) = write_str(key);
    if kptr <= 0 {
        return None;
    }
    // 值最长可以是 KV_BUF_CAP（模板就是这么用的），所以按上限读：栈上那点缓冲
    // 会把长模板截断，而截断是静默的。
    let buf = __alloc(KV_BUF_CAP);
    if buf <= 0 {
        return None;
    }
    let n = unsafe { host_kv_get(kptr, klen, buf, KV_BUF_CAP) };
    if n <= 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf as *const u8, n as usize) };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

// ---------------------------------------------------------------------------
// 事件入口
// ---------------------------------------------------------------------------

/// 处理一个事件。返回码语义见模块文档的表格。
#[no_mangle]
pub extern "C" fn on_event(ptr: i32, len: i32) -> i32 {
    // 1) 解析事件。
    let payload = match read_payload(ptr, len) {
        Some(bytes) => bytes,
        None => return 1,
    };
    let event: Event = match serde_json::from_slice(payload) {
        Ok(event) => event,
        Err(_) => {
            log(3, "tg-notify: 事件载荷不是合法的事件 JSON");
            return 1;
        }
    };

    // 2) 读渠道配置。没配置是操作员可修复的状态：返回明确的错误码而不是
    //    静默成功——静默成功会让"通知没发出去"变成一个谜。
    let Some(token) = kv_get_string("bot_token") else {
        log(
            2,
            "tg-notify: kv 里没有 bot_token；请在面板的插件 KV 编辑器里填写",
        );
        return 2;
    };
    let Some(chat_id) = kv_get_string("chat_id") else {
        log(
            2,
            "tg-notify: kv 里没有 chat_id；请在面板的插件 KV 编辑器里填写",
        );
        return 3;
    };

    // 3) 构造 sendMessage 请求。token 内嵌在 URL 里，所以日志只打 host。
    let text = render(&event);
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let body = json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
    });
    let body = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());

    let (method_ptr, method_len) = write_str("POST");
    let (url_ptr, url_len) = write_str(&url);
    let (body_ptr, body_len) = write_bytes(&body);
    if method_ptr <= 0 || url_ptr <= 0 || body_ptr <= 0 {
        log(3, "tg-notify: 分配请求缓冲失败");
        return 11;
    }
    // 4 KiB 的响应缓冲：sendMessage 的响应是一小段 JSON，4 KiB 足够看清 ok 字段。
    const RESP_CAP: i32 = 4096;
    let resp_ptr = __alloc(RESP_CAP);

    // 4) 发送。返回负数是宿主错误码：10 - code 落在 11..=19，与其它错误码错开。
    let n = unsafe {
        host_http_post(
            method_ptr, method_len, url_ptr, url_len, body_ptr, body_len, resp_ptr, RESP_CAP,
        )
    };
    if n < 0 {
        log(3, &format!("tg-notify: sendMessage 失败，宿主错误码 {n}（-1 越界 -2 非 https -3 非 POST -4 网络 -5 非 2xx -9 私有地址被拒）"));
        return 10 - n;
    }
    0
}

/// 经 `host_log` 打一条日志（level：0=debug 1=info 2=warn 3=error）。
fn log(level: i32, msg: &str) {
    let (ptr, len) = write_str(msg);
    if ptr > 0 {
        unsafe { host_log(level, ptr, len) };
    }
}
