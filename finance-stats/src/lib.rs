//! finance-stats —— 财务统计插件，monitor-hub wasm 插件 ABI v2 的参考实现。
//!
//! 职责：
//!
//! - 自持全部节点财务数据（价格 / 币种 / 计费周期 / 到期日），存在自己的
//!   `plugin_data` 命名空间里（宿主 node 表不再有这些列）；
//! - 与宿主节点表**对账**（每轮 tick、每次打开页面各一次）：补建缺的记录、
//!   删掉宿主已不存在的记录、按 `(id, created_at)` 认出 id 被复用而重置那条
//!   记录；拿不到宿主节点集合时一条都不动；
//! - 订阅宿主事件 `node_added` / `node_deleted`，启用期间新增与删除实时同步；
//! - 每小时 tick：用 `http_get` 从 Frankfurter 拉汇率并缓存，再扫描到期日
//!   （到期的在线节点按周期向后滚动；进入 `threshold_days` 窗口的经
//!   `emit_event` 发 `plugin_expiry_soon`，窗口内每天一次，靠记录里的
//!   `last_notified` 去重）。提醒阈值取自面板 kv（「渠道配置」弹窗里的
//!   `threshold_days`，见 `plugin.toml` 的 `[[kv]]`）；没配就用 `config`
//!   记录里留下的值、再退到默认 7 天；
//! - `render_page` 渲染统计页（年化续费成本 / 剩余价值 / 到期列表 / 编辑表）；
//!   首次渲染时若尚无汇率缓存，先同步拉一次（R4/KTD3）；
//! - `on_action` 处理页面交互（切币种 / 保存节点 / 立即刷新汇率）——其余动作
//!   路径不**隐式**拉取（切币种与手动刷新各自显式拉一次）。
//!
//! # 数据模型（plugin_data 的记录）
//!
//! | key | value |
//! |-----|-------|
//! | `config` | `{target_currency, threshold_days}` |
//! | `fx` | `{base, rates: {CUR: number}, fetched_at}` |
//! | `fx_status` | `{reason, attempted_at}`——最近一次拉取失败的原因与时间；成功即删（R5/KTD4） |
//! | `node:<id>` | `{name, price, currency, billing_cycle, expires_at, purchased_at, host_created_at, last_notified}` |
//!
//! `config` 里的 `threshold_days` 只是回退：面板 kv 的 `threshold_days` 优先
//! （`threshold_from_kv`），两者都没有才用默认 7 天。升级前的部署不必迁移——
//! 旧的 `config` 记录照常作数，操作员在「渠道配置」里填一次就换了来源。
//!
//! 时间一律向宿主要（`host_now`），日期算术交给 chrono 的 NaiveDate。
//! 时区用 UTC——与面板展示的本地日期可能差一天，这是已知取舍（宿主原 Logic
//! 用本地时区，插件在 wasm 里拿不到时区）。

use chrono::{DateTime, Months, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ptr;

// ---------------------------------------------------------------------------
// 宿主函数（ABI v2）：从名为 "host" 的 wasm import 模块导入。
// 签名与错误码见 hub 的 src/plugin/host.rs。
// ---------------------------------------------------------------------------

#[link(wasm_import_module = "host")]
extern "C" {
    /// level：0=debug 1=info 2=warn 3=error。
    #[link_name = "log"]
    fn host_log(level: i32, ptr: i32, len: i32);
    /// 当前 Unix 秒。
    #[link_name = "now"]
    fn host_now() -> i64;
    /// 读面板 kv（插件在「渠道配置」弹窗里填的值，命名空间
    /// `plugin.<plugin_id>:<key>`）：返回写入 out 的字节数，0 = 无值，
    /// -1 = 越界/非法 UTF-8。
    #[link_name = "kv_get"]
    fn host_kv_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32;
    /// 让宿主经 `__alloc` 分配响应缓冲并记住指针。
    #[link_name = "resp_alloc"]
    fn host_resp_alloc(cap: i32) -> i32;
    /// 只读 https GET。返回写入 resp 缓冲的字节数；负数是错误码。
    #[link_name = "http_get"]
    fn host_http_get(url_ptr: i32, url_len: i32, resp_ptr: i32, resp_cap: i32) -> i32;
    /// 只读节点基础信息：返回写入 out 的字节数（JSON 数组），负数是错误码。
    #[link_name = "nodes_query"]
    fn host_nodes_query(out_ptr: i32, out_cap: i32) -> i32;
    /// 发出一个 `plugin_` 前缀的事件。
    #[link_name = "emit_event"]
    fn host_emit_event(name_ptr: i32, name_len: i32, payload_ptr: i32, payload_len: i32) -> i32;
    /// 写一行插件数据：0 成功，-6 超限，-8 数据库失败。
    #[link_name = "data_put"]
    fn host_data_put(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> i32;
    /// 读一行插件数据：写入 out 的字节数，0 无此记录。
    #[link_name = "data_get"]
    fn host_data_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32;
    /// 删一行插件数据：0 成功。
    #[link_name = "data_delete"]
    fn host_data_delete(key_ptr: i32, key_len: i32) -> i32;
    /// 按前缀列出一段插件数据：写入 out 的字节数（JSON 数组），-8 数据库失败。
    #[link_name = "data_list"]
    fn host_data_list(prefix_ptr: i32, prefix_len: i32, out_ptr: i32, out_cap: i32) -> i32;
}

// ---------------------------------------------------------------------------
// 分配器与内存工具
// ---------------------------------------------------------------------------

static mut HEAP_NEXT: usize = 1024;

/// 宿主写事件/入参、`host_resp_alloc` 回程都走这里。
#[no_mangle]
pub extern "C" fn __alloc(cap: i32) -> i32 {
    if cap <= 0 {
        return 1024;
    }
    unsafe {
        let ptr = HEAP_NEXT;
        HEAP_NEXT += cap as usize + 8;
        ptr as i32
    }
}

fn write_bytes(bytes: &[u8]) -> (i32, i32) {
    let ptr = __alloc(bytes.len().max(1) as i32);
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    (ptr, bytes.len() as i32)
}

fn write_str(s: &str) -> (i32, i32) {
    write_bytes(s.as_bytes())
}

fn log(level: i32, msg: &str) {
    let (p, l) = write_str(msg);
    unsafe { host_log(level, p, l) };
}

/// 把一段字节当 UTF-8 读回；非法 UTF-8 用有损转换。
fn bytes_to_string(buf: &[u8]) -> String {
    String::from_utf8_lossy(buf).into_owned()
}

// ---------------------------------------------------------------------------
// plugin_data 包装
// ---------------------------------------------------------------------------

/// 缓冲大小：单条节点记录与整段列表都很小（百台机器量级）。
const BUF: usize = 256 * 1024;

fn data_put(key: &str, value: &str) -> bool {
    let (kp, kl) = write_str(key);
    let (vp, vl) = write_str(value);
    unsafe { host_data_put(kp, kl, vp, vl) == 0 }
}

fn data_get(key: &str) -> Option<String> {
    let (kp, kl) = write_str(key);
    let mut buf = vec![0u8; 16 * 1024];
    let n = unsafe { host_data_get(kp, kl, buf.as_mut_ptr() as i32, buf.len() as i32) };
    if n <= 0 {
        return None;
    }
    Some(bytes_to_string(&buf[..n as usize]))
}

fn data_delete(key: &str) {
    let (kp, kl) = write_str(key);
    unsafe { host_data_delete(kp, kl) };
}

/// `data_list` 回的一行。
#[derive(Deserialize)]
struct DataRow {
    key: String,
    data: String,
}

/// 列出 `prefix` 前缀的全部记录，返回 (key, data) 列表。宿主读回的字节写进调用
/// 方给的 `buf`：分配并清零 256 KiB 是这个插件最贵的一笔 fuel，一次调用里的几处
/// 宿主读先后使用同一块（读回的内容当场解析成 owned 数据，缓冲不再需要）。
///
/// `None` 表示**这次读没成功**（负错误码=超配额或数据库错、解析失败），与「一条
/// 记录都没有」（`Some(空)`）必须分开：对账拿它做写操作，把读失败当成空集合会把
/// 每一台机器的记录都改写成空白记录，价格全丢且无从恢复。宿主侧同一纪律见
/// [`host_nodes`]。
fn data_list(prefix: &str, buf: &mut [u8]) -> Option<Vec<(String, String)>> {
    let (pp, pl) = write_str(prefix);
    let n = unsafe { host_data_list(pp, pl, buf.as_mut_ptr() as i32, buf.len() as i32) };
    if n <= 0 {
        return None;
    }
    let text = bytes_to_string(&buf[..n as usize]);
    let rows: Vec<DataRow> = serde_json::from_str(&text).ok()?;
    Some(rows.into_iter().map(|r| (r.key, r.data)).collect())
}

// ---------------------------------------------------------------------------
// 面板 kv 配置（manifest 的 [[kv]] 声明，面板「渠道配置」弹窗写入）
// ---------------------------------------------------------------------------

/// 读一个面板 kv 项。无值（返回 0）、越界（负数）或非法 UTF-8 都归成 None。
fn kv_get_string(key: &str) -> Option<String> {
    let (kp, kl) = write_str(key);
    // 阈值只有几位数字；256 字节容纳任何合理输入，写超了被宿主截断后解析失败、
    // 回退默认——不会把半个数字当成阈值。
    let mut buf = [0u8; 256];
    let n = unsafe { host_kv_get(kp, kl, buf.as_mut_ptr() as i32, buf.len() as i32) };
    if n <= 0 {
        return None;
    }
    Some(bytes_to_string(&buf[..n as usize]))
}

// ---------------------------------------------------------------------------
// 领域模型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeFin {
    name: String,
    price: f64,
    currency: String,
    billing_cycle: String,
    expires_at: Option<String>,
    /// 一次性付费机器的购买日，用于剩余价值折算（周期机器不读）。
    #[serde(default)]
    purchased_at: Option<String>,
    /// 这条记录属于哪一台机器：建立时宿主 node 行的 `created_at`。宿主 id 会被
    /// SQLite 复用（删掉最大 id 的节点后新建的节点会拿到同一个 id），所以
    /// `node:<id>` 这个键本身认不出机器——值里的这一份才是身份：与宿主当前值
    /// 不一致就说明该 id 已属于另一台机器，记录必须重置。
    ///
    /// 升级前留下的记录没有这个字段（`None`），首次对账只采纳宿主当前值，
    /// 不重置——里面可能是操作员已经填好的价格。
    #[serde(default)]
    host_created_at: Option<i64>,
    /// 上次发到期提醒时该节点的 `days_left`。提醒在窗口内**每天一次**，靠它
    /// 去重：宿主每小时一拍，`days_left` 一天只减一次，值没变就是今天已经提醒过。
    /// 升级前留下的记录没有这个字段（`None`），进窗口时会照常提醒一次。
    #[serde(default)]
    last_notified: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Fx {
    base: String,
    rates: HashMap<String, f64>,
    fetched_at: i64,
}

/// 最近一次拉取汇率的失败记录（R5/KTD4）：成功时删掉这个键，失败时覆盖写入。
/// 只要它存在，页面提示条就会带上原因与尝试时间——不管有没有旧缓存。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FxStatus {
    reason: String,
    attempted_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    #[serde(default = "default_target")]
    target_currency: String,
    #[serde(default = "default_threshold")]
    threshold_days: i64,
}

fn default_target() -> String {
    "CNY".into()
}
fn default_threshold() -> i64 {
    7
}

impl Config {
    fn load() -> Config {
        data_get("config").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| Config {
            target_currency: default_target(),
            threshold_days: default_threshold(),
        })
    }
    fn save(&self) {
        if let Ok(s) = serde_json::to_string(self) {
            data_put("config", &s);
        }
    }

    /// 生效的提醒阈值。面板 kv 优先（操作员在「渠道配置」里改的那个），其次是
    /// `config` 记录里 `threshold_days`（升级前手工写库留下的值），最后是编译期
    /// 默认。回退链上少一环都不报错——没配过的部署照旧按默认 7 天提醒。
    ///
    /// 不去改写 `self.threshold_days`：`save` 会把它写回 `config` 记录，覆盖了
    /// 就等于让 kv 的值渗进另一个来源，之后删掉 kv 也回不到原样。
    fn threshold_days(&self) -> i64 {
        threshold_from_kv().unwrap_or(self.threshold_days)
    }
}

/// 面板 kv 里的 `threshold_days`。非正整数（0、负数、非数字、超长截断）一律当
/// 没配，回退到 `config` 记录：窗口判据是 `days_left <= 阈值`，0 或负数会让窗口
/// 变空，"配了个静默失效的值"比回退到能用的默认更难查。
fn threshold_from_kv() -> Option<i64> {
    kv_get_string("threshold_days").and_then(|s| s.trim().parse::<i64>().ok()).filter(|d| *d > 0)
}

/// 周期名 → 月数 → 下拉里的中文展示标签，顺序即下拉里的展示顺序。统计口径、
/// 下拉取值与展示文案共用这一份，才不会出现「选得到但算不出」的缺口，也不会
/// 让一张独立的标签表随时间漂移。
const CYCLE_MONTHS: [(&str, u32, &str); 6] = [
    ("monthly", 1, "月付"),
    ("quarterly", 3, "季付"),
    ("semiannual", 6, "半年付"),
    ("yearly", 12, "年付"),
    ("biennial", 24, "两年付"),
    ("triennial", 36, "三年付"),
];

/// 无周期的一次性付费：不在 `CYCLE_MONTHS` 里，故 `cycle_months` 返回 None。
const ONCE: &str = "once";
/// `once` 的中文展示标签，与 `CYCLE_MONTHS` 里的标签同一处定义。
const ONCE_LABEL: &str = "一次性";

/// 免费机器：同样不在 `CYCLE_MONTHS` 里，但它与 `once` 不是一回事——一次性
/// 付费仍按购买日折算剩余价值，免费的什么都不算（价格列也不作数）。
const FREE: &str = "free";
/// `free` 的中文展示标签。
const FREE_LABEL: &str = "免费";

/// 这台机器是否不算钱：周期标成免费，或价格为零。汇总、剩余价值与到期表的
/// 「免费」备注共用这一处判定，免得三处各写一个条件。
fn is_free(n: &NodeFin) -> bool {
    n.billing_cycle == FREE || n.price <= 0.0
}

/// 计费周期 → 月数：全插件唯一的周期真源（下拉取值与标签也由它派生）。`once`
/// 与 `free` 无周期（不滚动、不进年化）。
fn cycle_months(cycle: &str) -> Option<u32> {
    CYCLE_MONTHS.iter().find(|(name, _, _)| *name == cycle).map(|(_, months, _)| *months)
}

fn today() -> NaiveDate {
    let secs = unsafe { host_now() };
    DateTime::from_timestamp(secs, 0)
        .map(|d| d.date_naive())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 1, 1).unwrap())
}

/// 节点记录键。
fn node_key(id: i64) -> String {
    format!("node:{id}")
}

fn load_node(id: i64) -> Option<NodeFin> {
    data_get(&node_key(id)).and_then(|s| serde_json::from_str(&s).ok())
}

fn save_node(id: i64, n: &NodeFin) {
    if let Ok(s) = serde_json::to_string(n) {
        data_put(&node_key(id), &s);
    }
}

/// 插件自己存的全部节点记录。`None` = 这次没读成功（见 [`data_list`]）：调用方
/// 必须把它当成「这轮拿不到数据」，绝不能当空集合。
fn all_nodes(buf: &mut [u8]) -> Option<Vec<(i64, NodeFin)>> {
    let rows = data_list("node:", buf)?;
    Some(
        rows.into_iter()
            .filter_map(|(key, data)| {
                let id = key.strip_prefix("node:")?.parse::<i64>().ok()?;
                let n = serde_json::from_str::<NodeFin>(&data).ok()?;
                Some((id, n))
            })
            .collect(),
    )
}

/// 宿主节点表的一份快照项，也是 `nodes_query` 应答体的元素。用定长结构体而不是
/// `serde_json::Value`：后者每个对象都是一棵 BTreeMap，百台机器量级下白付一大笔
/// 解析 fuel。
#[derive(Deserialize)]
struct HostNode {
    id: i64,
    name: String,
    #[serde(default)]
    online: bool,
    /// 宿主 node 行的 `created_at`，机器身份的另一半（见 [`NodeFin::host_created_at`]）。
    /// `None` = 宿主没回这个字段，此时不比对身份，只补缺。
    #[serde(default)]
    created_at: Option<i64>,
}

/// 读宿主节点表。`None` 表示**查询失败**（负错误码，含结果放不下），与「确实没有
/// 节点」（`Some(空)`）必须分开：对账拿不到存活集合时若把它当成空集合，会把所有
/// 节点的记录全删掉；到期扫描同理，不知道谁在线就不该滚动任何日期。
///
/// 读回的字节写进调用方给的 `buf`，理由见 [`data_list`]。
fn host_nodes(buf: &mut [u8]) -> Option<Vec<HostNode>> {
    let n = unsafe { host_nodes_query(buf.as_mut_ptr() as i32, buf.len() as i32) };
    if n < 0 {
        return None;
    }
    Some(serde_json::from_str(&bytes_to_string(&buf[..n as usize])).unwrap_or_default())
}

// ---------------------------------------------------------------------------
// 与宿主节点表对账（KTD10：从「一次性导入」演进为持续对账）
// ---------------------------------------------------------------------------

/// 空白财务记录：宿主刚建好的节点，或 id 被复用后属于新机器的那条记录。
fn blank_node(name: String, created_at: Option<i64>) -> NodeFin {
    NodeFin {
        name,
        price: 0.0,
        currency: "USD".into(),
        billing_cycle: "yearly".into(),
        expires_at: None,
        purchased_at: Some(today().to_string()),
        host_created_at: created_at,
        last_notified: None,
    }
}

/// 把插件里存的节点记录按 `host` 这份宿主快照对齐，并返回对齐后的记录集合
/// （调用方拿它直接算统计，不必再读一遍 `plugin_data`——每次读都要按节点量级
/// 分配缓冲，而那是这个插件最贵的一笔 fuel 开销）：
///
/// - 宿主有、插件无 → 建空白记录；
/// - 身份相符 → 不动（价格与到期日是操作员填的，对账绝不覆盖）；
/// - 记录缺身份（升级前留下的）→ 采纳宿主当前值，不重置；
/// - 身份不符 → 同一个 id 已被 SQLite 复用给另一台机器，重置为空白记录；
/// - 宿主已无这个 id → 删掉记录。
///
/// 返回顺序即 `host` 的顺序，也就是 hub 面板节点列表的顺序：页面行序与统计口径
/// 都建立在这个顺序上，操作者在插件页看到的行序才与面板一致。这一轮新建的记录
/// 同样落在它的宿主位置上，不会先挂在末尾、下次渲染再跳回中间。
fn reconcile_with(host: &[HostNode], known: Vec<(i64, NodeFin)>) -> Vec<(i64, NodeFin)> {
    let mut by_id: HashMap<i64, NodeFin> = known.into_iter().collect();
    let mut live: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for h in host {
        live.insert(h.id);
        match by_id.get(&h.id) {
            None => {
                let blank = blank_node(h.name.clone(), h.created_at);
                save_node(h.id, &blank);
                by_id.insert(h.id, blank);
            }
            Some(record) => {
                // 宿主没回身份就不猜：留着记录，等宿主升级后下一轮再比。
                let Some(created_at) = h.created_at else { continue };
                match record.host_created_at {
                    None => {
                        let mut adopted = record.clone();
                        adopted.host_created_at = Some(created_at);
                        save_node(h.id, &adopted);
                        by_id.insert(h.id, adopted);
                    }
                    Some(stored) if stored != created_at => {
                        let blank = blank_node(h.name.clone(), Some(created_at));
                        save_node(h.id, &blank);
                        by_id.insert(h.id, blank);
                    }
                    Some(_) => {}
                }
            }
        }
    }
    let stale: Vec<i64> = by_id.keys().copied().filter(|id| !live.contains(id)).collect();
    for id in stale {
        data_delete(&node_key(id));
        by_id.remove(&id);
    }
    // 按宿主顺序输出：`nodes_query` 回的正是 hub 面板的 `ORDER BY sort, id`
    // （用户拖拽序优先、数字 id 兜底），照搬它页面行序才与面板一致。不能按记录键
    // 排——那是字符串序（`node:10` 跑到 `node:2` 前），而且完全忽略 `sort`；更
    // 不能用 `HashMap` 的迭代序，它每个实例都不一样。
    //
    // 此时 `by_id` 的键集恰好等于 `host` 的 id 集：宿主已无的上面刚删光，
    // 每个宿主 id 又都在循环里建过或确认过记录，所以这里既不漏也不多。
    let mut out: Vec<(i64, NodeFin)> = Vec::with_capacity(by_id.len());
    for h in host {
        if let Some(n) = by_id.remove(&h.id) {
            out.push((h.id, n));
        }
    }
    out
}

/// 读插件自己的记录与宿主节点表并对账，返回对账后的记录集合。两种读失败的处理
/// 不同：
///
/// - 插件自己的记录读不出来 → `None`：这轮**什么都不能写**（把读失败当成空集合
///   会把每台机器都改写成空白记录），调用方按「拿不到数据」处理；
/// - 宿主节点表读不出来 → 原样返回已有记录、不做增删改（见 [`host_nodes`]）。
///   这条路上没有宿主顺序可用，行序退回 `data_list` 的记录键序——拿不到面板顺序
///   时只能如此，下一轮读成功即恢复。
fn reconcile() -> Option<Vec<(i64, NodeFin)>> {
    let mut buf = vec![0u8; BUF];
    let Some(known) = all_nodes(&mut buf) else {
        log(3, "finance-stats: 读插件记录失败,本轮跳过对账");
        return None;
    };
    match host_nodes(&mut buf) {
        Some(host) => Some(reconcile_with(&host, known)),
        None => {
            log(2, "finance-stats: nodes_query 失败,本轮跳过节点对账");
            Some(known)
        }
    }
}

// ---------------------------------------------------------------------------
// 汇率（Frankfurter，KTD2/KTD5）
// ---------------------------------------------------------------------------

/// http 负错误码 → 可读的失败原因（码表见 hub 的 src/plugin/host_funcs.rs）。
/// `0` 不是错误码——宿主约定它表示调用成功但应答体为空（http_get 返回写入的
/// 字节数），所以它有自己的文案，不能落进「错误码 0」那句没有信息量的兜底。
fn http_error_reason(code: i32) -> String {
    match code {
        -1 => "宿主侧读写失败（参数越界/非 UTF-8 或写回失败）".to_owned(),
        -2 => "目标地址不是 https，宿主拒绝".to_owned(),
        -4 => "网络请求失败或超时".to_owned(),
        -5 => "汇率服务返回非 2xx 响应".to_owned(),
        -9 => "目标地址解析到私有/保留网段，宿主拒绝".to_owned(),
        0 => "汇率服务返回了空响应".to_owned(),
        other => format!("宿主返回错误码 {other}"),
    }
}

/// 记下这次失败的原因与时间；下一次成功拉取会把它删掉。
/// 拉取有多个入口（页面首屏 / 每小时 tick / 手动刷新），记录放在 `refresh_fx`
/// 内部，谁触发都留痕——页面才不会只看得见"没拉过"而看不见"一直拉失败"。
fn record_fx_failure(reason: String) {
    let st = FxStatus { reason, attempted_at: unsafe { host_now() } };
    if let Ok(s) = serde_json::to_string(&st) {
        data_put("fx_status", &s);
    }
}

fn clear_fx_failure() {
    data_delete("fx_status");
}

fn load_fx_status() -> Option<FxStatus> {
    data_get("fx_status").and_then(|s| serde_json::from_str(&s).ok())
}

/// Unix 秒 → 页面上的时间文案（与汇率更新时间同格式）。
fn fmt_ts(secs: i64) -> String {
    DateTime::from_timestamp(secs, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default()
}

/// 拉一次汇率，以目标币种为 base。成功则更新缓存、清掉失败记录并返回 true；
/// 失败则把失败原因与尝试时间写入 `fx_status` 并返回 false。
fn refresh_fx(cfg: &Config) -> bool {
    let base = &cfg.target_currency;
    let url = format!("https://api.frankfurter.dev/v1/latest?base={base}");
    log(1, &format!("finance-stats: 拉取汇率 {url}"));
    let (up, ul) = write_str(&url);
    let mut buf = vec![0u8; 64 * 1024];
    let n = unsafe { host_http_get(up, ul, buf.as_mut_ptr() as i32, buf.len() as i32) };
    if n <= 0 {
        let reason = http_error_reason(n);
        log(2, &format!("finance-stats: 汇率拉取失败（{n}）：{reason}"));
        record_fx_failure(reason);
        return false;
    }
    let body = bytes_to_string(&buf[..n as usize]);
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            log(2, &format!("finance-stats: 汇率响应不是 JSON: {e}"));
            record_fx_failure(format!("汇率响应不是合法 JSON：{e}"));
            return false;
        }
    };
    let rates: HashMap<String, f64> =
        v.get("rates").and_then(|r| serde_json::from_value(r.clone()).ok()).unwrap_or_default();
    if rates.is_empty() {
        log(2, "finance-stats: 汇率响应没有 rates");
        record_fx_failure("汇率响应里没有 rates 字段".to_owned());
        return false;
    }
    let fx = Fx { base: base.clone(), rates, fetched_at: unsafe { host_now() } };
    if let Ok(s) = serde_json::to_string(&fx) {
        data_put("fx", &s);
        clear_fx_failure();
        return true;
    }
    // 防御分支：`Fx` 全是可序列化的基础类型，这里实际不会失败，故测试覆盖
    // 不到（不是死代码——将来给 `Fx` 加上不可序列化的字段时不至于把失败吞掉）。
    record_fx_failure("汇率缓存序列化失败，未写入".to_owned());
    false
}

fn load_fx() -> Option<Fx> {
    data_get("fx").and_then(|s| serde_json::from_str(&s).ok())
}

/// `refresh_fx` 失败时的 toast 文案。以真实状态为准，与页面提示条同源：
/// 有没有旧缓存决定「统计沿用缓存」还是「统计暂缺」，原因取插件刚记下的那份
/// （失败路径必然写了 `fx_status`）。两者都拿不到时退回一句不带细节的失败
/// 提示，不编造缓存状态。
fn refresh_fx_failure_toast() -> String {
    let reason = load_fx_status().map(|st| st.reason);
    match (load_fx().is_some(), reason) {
        (true, Some(reason)) => format!("汇率刷新失败，统计沿用最近一次缓存：{reason}"),
        (false, Some(reason)) => {
            format!("汇率刷新失败：{reason}（尚未成功拉取过汇率，统计暂缺）")
        }
        (_, None) => "汇率刷新失败".to_owned(),
    }
}

/// 把一个金额从 `from` 币种换算到目标币种。汇率表以目标币种为 base，
/// `rates[from]` 是「1 目标币 = 多少 from」，故换算为除法。缺汇率返回 None。
fn convert(fx: &Fx, amount: f64, from: &str) -> Option<f64> {
    if from == fx.base {
        return Some(amount);
    }
    fx.rates.get(from).filter(|r| **r != 0.0).map(|r| amount / r)
}

// ---------------------------------------------------------------------------
// 统计
// ---------------------------------------------------------------------------

/// 一台机器的年化成本（原币种）。免费的与 once 的按 R10 处理。
fn annual_cost(n: &NodeFin) -> Option<f64> {
    if is_free(n) {
        return None; // 免费的不计年化，价格列写多少都不作数
    }
    let months = cycle_months(&n.billing_cycle)?;
    Some(n.price * (12.0 / months as f64))
}

/// 剩余价值（原币种）。周期机器按周期内未消耗比例；once 按购买日折算；
/// 无到期日视为 0。
fn remaining_value(n: &NodeFin, today: NaiveDate) -> Option<f64> {
    // 免费的不算剩余（也不进 `once` 那条按购买日折算的分支——它压根没付过钱）。
    if is_free(n) {
        return None;
    }
    let expires = n.expires_at.as_deref().and_then(|d| d.parse::<NaiveDate>().ok())?;
    let days_left = (expires - today).num_days();
    if days_left <= 0 {
        return Some(0.0);
    }
    match cycle_months(&n.billing_cycle) {
        Some(months) => {
            // 周期长度按 30 天/月近似（与面板观感一致，不需要公历精确）。
            let cycle_days = (months as f64) * 30.0;
            Some(n.price * (days_left as f64 / cycle_days).min(1.0))
        }
        None => {
            // once：按购买日到到期日的总跨度折算。
            let purchased = n.purchased_at.as_deref().and_then(|d| d.parse::<NaiveDate>().ok());
            match purchased {
                Some(p) => {
                    let total = (expires - p).num_days().max(1) as f64;
                    Some(n.price * (days_left as f64 / total).min(1.0))
                }
                None => Some(0.0),
            }
        }
    }
}

/// 汇总：返回 (年化总成本, 剩余总价值) 在目标币种下的值；任一节点缺汇率
/// 则该项跳过。fx 为 None 时返回 None（汇率不可用）。
fn totals(fx: Option<&Fx>, nodes: &[(i64, NodeFin)], today: NaiveDate) -> Option<((f64, f64), Vec<String>)> {
    let fx = fx?;
    let mut annual = 0.0;
    let mut remaining = 0.0;
    // 任一节点缺汇率则该项跳过；列出被跳过的币种让面板提示用户。
    let mut missing: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (_, n) in nodes {
        if let Some(c) = annual_cost(n) {
            match convert(fx, c, &n.currency) {
                Some(v) => annual += v,
                None => {
                    missing.insert(n.currency.clone());
                }
            }
        }
        if let Some(r) = remaining_value(n, today) {
            match convert(fx, r, &n.currency) {
                Some(v) => remaining += v,
                None => {
                    missing.insert(n.currency.clone());
                }
            }
        }
    }
    Some(((annual, remaining), missing.into_iter().collect()))
}

// ---------------------------------------------------------------------------
// 到期扫描（周期性）
// ---------------------------------------------------------------------------

/// 滚动到期日：到期日已过但节点仍在线的，按周期向后滚到未来。返回是否改动。
fn roll_if_online(id: i64, n: &mut NodeFin, online: &[i64], today: NaiveDate) -> bool {
    if !online.contains(&id) {
        return false;
    }
    let Some(months) = cycle_months(&n.billing_cycle) else {
        return false;
    };
    let Some(exp) = n.expires_at.as_deref().and_then(|d| d.parse::<NaiveDate>().ok()) else {
        return false;
    };
    if exp >= today {
        return false;
    }
    let mut next = exp;
    let step = Months::new(months);
    while next < today {
        match next.checked_add_months(step) {
            Some(x) => next = x,
            None => return false,
        }
    }
    n.expires_at = Some(next.to_string());
    // 滚到新周期，提醒从头计：旧周期留下的天数可能恰好撞上新窗口里的某一天，
    // 那会让新周期的第一次提醒被吞掉。
    n.last_notified = None;
    true
}

/// 一拍的全部周期工作：刷新汇率 → 对账 → 扫描到期。
fn tick() {
    let cfg = Config::load();
    // 阈值整轮读一次：kv 是一次跨 wasm 边界的调用，放进节点循环里就变成每台机器
    // 各来一次，换来的是同一个值。
    let threshold = cfg.threshold_days();
    if refresh_fx(&cfg) {
        log(1, "finance-stats: 汇率已更新");
    }
    let today = today();
    // 一次 nodes_query 同时供对账（谁存在）与到期扫描（谁在线）使用；对账返回的那
    // 一份记录接着用来扫描，不再读第二遍 plugin_data。
    let mut buf = vec![0u8; BUF];
    // 自己那份记录读不出来：对账与扫描都要它，整轮不动。
    let Some(known) = all_nodes(&mut buf) else {
        log(3, "finance-stats: 读插件记录失败,本轮跳过对账与到期扫描");
        return;
    };
    let (nodes, online) = match host_nodes(&mut buf) {
        Some(host) => {
            let online: Vec<i64> = host.iter().filter(|n| n.online).map(|n| n.id).collect();
            (reconcile_with(&host, known), online)
        }
        None => {
            // 拿不到在线集合只影响**滚动**：到期提醒读的全是插件自己的数据
            // （expires_at、阈值与去重状态），照常发。
            log(2, "finance-stats: nodes_query 失败,本轮跳过到期滚动");
            (known, Vec::new())
        }
    };
    for (id, mut n) in nodes {
        if roll_if_online(id, &mut n, &online, today) {
            log(1, &format!("finance-stats: 节点 {} 到期日滚动到 {:?}", n.name, n.expires_at));
            save_node(id, &n);
            continue; // 刚滚动的节点 days_left 变大，本轮不再提醒
        }
        let Some(exp) = n.expires_at.as_deref().and_then(|d| d.parse::<NaiveDate>().ok()) else {
            continue;
        };
        let days_left = (exp - today).num_days();
        // 窗口内（`0 < days_left <= 阈值`）每天提醒一次；已过期（<=0）不提醒——
        // 那是滚动或人工处理的事。宿主每小时一拍，而 `days_left` 一天只减一次，
        // 用记录里上次提醒的天数去重，否则一天会轰出 24 条。
        if days_left > 0 && days_left <= threshold && n.last_notified != Some(days_left) {
            let payload = json!({
                "node_id": id,
                "name": n.name,
                "expires_at": n.expires_at,
                "days_left": days_left,
                "threshold_days": threshold,
            });
            let (np, nl) = write_str("plugin_expiry_soon");
            let (pp, pl) = write_str(&payload.to_string());
            unsafe { host_emit_event(np, nl, pp, pl) };
            // 先发后记：记失败最多下一轮重发一次；反过来的话，这一天的提醒就
            // 整个被吞掉了。
            n.last_notified = Some(days_left);
            save_node(id, &n);
        }
    }
}

// ---------------------------------------------------------------------------
// 页面渲染（U5）
// ---------------------------------------------------------------------------

/// 支持的展示币种及各自的符号：面板原有的六个 + Frankfurter 覆盖的常见币种。
/// 币种下拉的文案、汇总金额与价格列的前缀都从这一份派生——符号只此一处，
/// 三处展示不会各自漂移。日元与人民币都写 ¥ 时分不清，故日元用 JP¥。
const CURRENCIES: [(&str, &str); 12] = [
    ("CNY", "¥"),
    ("USD", "$"),
    ("EUR", "€"),
    ("GBP", "£"),
    ("JPY", "JP¥"),
    ("CAD", "C$"),
    ("HKD", "HK$"),
    ("AUD", "A$"),
    ("CHF", "CHF"),
    ("SGD", "S$"),
    ("KRW", "₩"),
    ("INR", "₹"),
];

/// 币种 → 符号。表外币种（历史 config 里可能留着的取值）没有符号。
fn currency_symbol(code: &str) -> Option<&'static str> {
    CURRENCIES.iter().find(|(c, _)| *c == code).map(|(_, symbol)| *symbol)
}

/// 价格列显示的前缀：认得的币种给符号，认不得的退回代码本身——总比什么都不放
/// 好读，那串数字至少还认得出是哪种钱。
fn currency_prefix(code: &str) -> &str {
    currency_symbol(code).unwrap_or(code)
}

/// 金额文案：认得的币种带符号（`¥123.45`），认不得的退回「金额 + 代码」
/// （`123.45 XXX`）——展示币种可以是表外的历史取值，只印一个数字会读不出是
/// 什么钱。
fn money(code: &str, amount: f64) -> String {
    match currency_symbol(code) {
        Some(symbol) => format!("{symbol}{amount:.2}"),
        None => format!("{amount:.2} {code}"),
    }
}

/// 币种下拉的选项：展示「¥ CNY」、提交 `CNY`（KTD5 的 `{value,label}` 形态）。
fn currency_options() -> Vec<Value> {
    CURRENCIES
        .iter()
        .map(|(code, symbol)| json!({ "value": code, "label": format!("{symbol} {code}") }))
        .collect()
}

/// 支持的计费周期下拉选项：`CYCLE_MONTHS` 的全部周期（同序）+ `once` + `free`。
/// 与 `cycle_months` 的覆盖集合同源——下拉里选得到、统计口径就认得出，
/// 自由文本时代拼错周期（如 `montly`）会被静默剔出年化汇总。
///
/// 每项是 `{value, label}`：面板展示 label、提交 value。value 仍是统计口径
/// 认得的那些小写字面量，label 直接取自 `CYCLE_MONTHS` 那一份表，不另立一张
/// 会漂移的对照表。
fn cycle_options() -> Vec<Value> {
    CYCLE_MONTHS
        .iter()
        .map(|(name, _, label)| json!({ "value": name, "label": label }))
        .chain(
            [(ONCE, ONCE_LABEL), (FREE, FREE_LABEL)]
                .into_iter()
                .map(|(name, label)| json!({ "value": name, "label": label })),
        )
        .collect()
}

/// 存下来但统计认不出的计费周期（历史数据里的拼写错误等），去重后按字典序
/// 返回。`cycle_months` 认不出就返回 None，那台机器会被静默剔出年化成本——
/// 列出来让操作员看得见。（`once` 与 `free` 是认得出的，不算。）
fn unrecognised_cycles(nodes: &[(i64, NodeFin)]) -> Vec<String> {
    let mut bad: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (_, n) in nodes {
        if n.billing_cycle != ONCE && n.billing_cycle != FREE && cycle_months(&n.billing_cycle).is_none() {
            bad.insert(n.billing_cycle.clone());
        }
    }
    bad.into_iter().collect()
}

/// 给页面描述挂一次性提示（KTD2：文案由插件提供，前端在 action 响应里弹一次）。
fn with_toast(mut page: Value, kind: &str, text: &str) -> Value {
    if let Some(obj) = page.as_object_mut() {
        obj.insert("toast".into(), json!({ "kind": kind, "text": text }));
    }
    page
}

fn build_page(allow_fetch: bool) -> Value {
    let cfg = Config::load();
    // 对账后的那一份记录同时供统计与两张表使用：每读一次 plugin_data 都要按
    // 节点量级分配缓冲，那是这个插件最贵的 fuel 开销，一次调用只读一次。
    let (nodes, unreadable) = match reconcile() {
        Some(nodes) => (nodes, false),
        // 记录读不出来（超配额/数据库错）时页面不能装作「一台机器都没有」：
        // 空表加一条说明，比一屏 0 元更像实话。
        None => (Vec::new(), true),
    };
    let today = today();
    // KTD3：拉取只发生在页面打开且无缓存时。动作路径（保存）不隐式拉——
    // 否则每次保存都要同步等一次网络往返。已有缓存也不拉，陈旧由 tick 兜底。
    let mut fx = load_fx();
    if fx.is_none() && allow_fetch && refresh_fx(&cfg) {
        // refresh_fx 返回 true 才刚写过缓存，此时重读是必然命中；失败时那次
        // data_get 是白跑一趟的宿主边界穿越。
        fx = load_fx();
    }
    let failure = load_fx_status();
    let target = cfg.target_currency.clone();

    let mut blocks: Vec<Value> = Vec::new();

    // 记录读不出来时先说明白：下面两张表会是空的，不说就成了「一台机器都没有」。
    if unreadable {
        blocks.push(json!({
            "type": "notice",
            "kind": "warning",
            "text": "读不到插件存的节点记录（可能超出单次读取上限或数据库出错），本轮没有对账，\
                     下面的表与统计暂缺；记录本身没有被改动，下次能读到时照旧",
        }));
    }

    // 汇率状态提示：成功过就报基准与更新时间，否则 warn。只要存在失败记录
    // （不管有没有旧缓存）就把原因与尝试时间并进来（R5/KTD4）。
    match &fx {
        Some(f) => {
            blocks.push(json!({
                "type": "notice",
                "text": format!("汇率基准 {}，更新时间 {}", f.base, fmt_ts(f.fetched_at)),
            }));
            if let Some(st) = &failure {
                blocks.push(json!({
                    "type": "notice",
                    "kind": "warning",
                    "text": format!(
                        "最近一次刷新 {} 失败：{}",
                        fmt_ts(st.attempted_at), st.reason
                    ),
                }));
            }
        }
        None => {
            let text = match &failure {
                Some(st) => format!(
                    "汇率不可用——尚未成功拉取过汇率，统计暂缺；最近一次尝试 {} 失败：{}",
                    fmt_ts(st.attempted_at), st.reason
                ),
                None => "汇率不可用——尚未成功拉取过汇率，统计暂缺".to_owned(),
            };
            blocks.push(json!({ "type": "notice", "kind": "warning", "text": text }));
        }
    }

    // 汇总统计与币种切换合成一处：第一格是展示币种下拉、后两格是金额，操作者
    // 一眼能把「哪两个数」和「按什么币种算的」对上。汇率不可用时金额印「—」，
    // 但下拉照常给——否则没缓存的部署连币种都换不了。
    let (annual, remaining) = match totals(fx.as_ref(), &nodes, today) {
        Some(((annual, remaining), missing)) => {
            if !missing.is_empty() {
                blocks.push(json!({
                    "type": "notice",
                    "kind": "warning",
                    "text": format!(
                        "部分节点币种未在 Frankfurter 汇率表中（{}），未计入汇总",
                        missing.join("、")
                    ),
                }));
            }
            (money(&target, annual), money(&target, remaining))
        }
        None => ("—".to_owned(), "—".to_owned()),
    };
    blocks.push(json!({
        "type": "stat",
        "items": [
            {"label": "展示币种", "select": {
                "value": target,
                "options": currency_options(),
                "action": "set_currency",
            }},
            {"label": "年化续费总成本", "value": annual},
            {"label": "剩余总价值", "value": remaining},
        ],
    }));

    // 存下来但统计认不出的周期会让那台机器静默掉出年化成本（`cycle_months`
    // 返回 None）。只提醒，统计口径一律不变——这条提示与上面的币种缺失提示
    // 同类，故紧挨着它放；汇率不可用时同样要给（与汇率无关）。
    let bad_cycles = unrecognised_cycles(&nodes);
    if !bad_cycles.is_empty() {
        blocks.push(json!({
            "type": "notice",
            "kind": "warning",
            "text": format!(
                "部分节点的计费周期无法识别（{}），未计入年化成本",
                bad_cycles.join("、")
            ),
        }));
    }

    // 到期窗口列表。
    let window = cfg.threshold_days();
    let mut due: Vec<Value> = Vec::new();
    let mut all: Vec<Value> = Vec::new();
    for (id, n) in &nodes {
        let days_left =
            n.expires_at.as_deref().and_then(|d| d.parse::<NaiveDate>().ok()).map(|e| (e - today).num_days());
        let free = is_free(n);
        all.push(json!({
            "id": id,
            "name": &n.name,
            "price": n.price,
            // 价格列的前缀（币种符号）与同一行的币种绑在一起发给面板：前端照
            // 字段声明里的 prefix_key 取它，不用认识币种代码。
            "price_symbol": currency_prefix(&n.currency),
            "currency": &n.currency,
            "billing_cycle": &n.billing_cycle,
            "expires_at": &n.expires_at,
            "free": free,
        }));
        if let Some(d) = days_left {
            if d >= 0 && d <= window {
                due.push(json!({
                    "name": &n.name,
                    "expires_at": &n.expires_at,
                    "days_left": d,
                    "free": free,
                }));
            }
        }
    }
    due.sort_by_key(|v| v.get("days_left").and_then(|d| d.as_i64()).unwrap_or(i64::MAX));
    blocks.push(json!({
        "type": "table",
        "title": format!("{window} 天内到期（{}）", due.len()),
        "columns": ["节点", "到期日", "剩余天数", "备注"],
        "rows": due.iter().map(|v| json!([
            v["name"], v["expires_at"], v["days_left"],
            if v["free"].as_bool().unwrap_or(false) { "免费" } else { "" }
        ])).collect::<Vec<_>>(),
    }));

    // 全量编辑表（价格/币种/周期/到期日）。
    // 字段声明用新式对象形态（KTD1）：带中文列头、控件类型与下拉选项。
    // 价格是 `money`：面板按钱渲染它（右对齐、两位小数），前缀取同行 `price_symbol`
    // 那一列——符号由插件算好，面板照样不认识币种代码。
    blocks.push(json!({
        "type": "form",
        "title": "节点财务数据",
        "action": "save_node",
        "fields": [
            {"name": "name", "label": "节点名", "type": "text"},
            {"name": "price", "label": "价格", "type": "money", "prefix_key": "price_symbol"},
            {"name": "currency", "label": "币种", "type": "select", "options": currency_options()},
            {"name": "billing_cycle", "label": "计费周期", "type": "select", "options": cycle_options()},
            {"name": "expires_at", "label": "到期日", "type": "date"},
        ],
        "rows": all,
    }));

    json!({ "title": "财务统计", "blocks": blocks })
}

// ---------------------------------------------------------------------------
// 页面交互（U5）
// ---------------------------------------------------------------------------

fn handle_action(input: &str) -> Value {
    let req: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    let action = req.get("action").and_then(|a| a.as_str()).unwrap_or("");
    match action {
        "set_currency" => {
            let mut cfg = Config::load();
            if let Some(cur) = req.get("value").and_then(|v| v.as_str()) {
                cfg.target_currency = cur.to_owned();
                cfg.save();
                refresh_fx(&cfg);
            }
            with_toast(build_page(false), "success", "已切换币种")
        }
        "refresh_fx" => {
            let cfg = Config::load();
            // 拉取结果决定提示文案：失败时不谎报成功，用户才知道统计为什么没动。
            // 失败文案与同一次响应里的页面提示条同源（有没有缓存、刚记下的原因）。
            let (kind, text) = if refresh_fx(&cfg) {
                ("success", "汇率已刷新".to_owned())
            } else {
                ("error", refresh_fx_failure_toast())
            };
            with_toast(build_page(false), kind, &text)
        }
        "save_node" => {
            let Some(id) = req.get("id").and_then(|v| v.as_i64()) else {
                return with_toast(build_page(false), "error", "未指定要保存的节点");
            };
            let Some(mut n) = load_node(id) else {
                return with_toast(build_page(false), "error", "节点记录不存在，未保存");
            };
            if let Some(v) = req.get("name").and_then(|v| v.as_str()) {
                n.name = v.to_owned();
            }
            if let Some(v) = req.get("price").and_then(|v| v.as_f64()) {
                n.price = v;
            }
            if let Some(v) = req.get("currency").and_then(|v| v.as_str()) {
                n.currency = v.to_owned();
            }
            if let Some(v) = req.get("billing_cycle").and_then(|v| v.as_str()) {
                n.billing_cycle = v.to_owned();
            }
            if let Some(v) = req.get("expires_at").and_then(|v| v.as_str()) {
                n.expires_at = if v.is_empty() { None } else { Some(v.to_owned()) };
            }
            save_node(id, &n);
            with_toast(build_page(false), "success", "已保存")
        }
        _ => build_page(false),
    }
}

// ---------------------------------------------------------------------------
// 导出
// ---------------------------------------------------------------------------

/// 宿主的节点生命周期事件。形状与 hub `notification_bus::Event` 的序列化一致：
/// 按 `type` 标签分派、字段按名解析、多余字段忽略（词表扩展不会弄坏旧插件）。
/// manifest 只订阅这两个；别的 `type` 解析失败，静默忽略。
///
/// 定长枚举而不是 `serde_json::Value`：事件派发的 fuel 预算只有
/// `plugin.fuel_limit`（默认 1e6），是页面/tick 那一档的 1/20，而 `Value` 给每个
/// 对象建一棵 BTreeMap，白付一笔解析开销。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum NodeEvent {
    /// 节点行被创建：事件是「这个 id 刚属于一台新机器」的权威声明。
    NodeAdded {
        node_id: i64,
        #[serde(default)]
        name: String,
        created_at: Option<i64>,
    },
    /// 节点行被删除。`name` 本插件不读，故不解析。
    NodeDeleted { node_id: i64, created_at: Option<i64> },
}

/// 宿主事件入口：`node_added` / `node_deleted`，启用期间实时同步。
///
/// 这里**只处理事件里那一台机器**，不做全量对账——事件预算见 [`NodeEvent`]，
/// 全量对账会直接烧穿。漏派的事件由 tick 与页面渲染兜底。
#[no_mangle]
pub extern "C" fn on_event(ptr: i32, len: i32) -> i32 {
    handle_node_event(&read_input(ptr, len));
    0
}

fn handle_node_event(input: &str) {
    // 面板的「测试」会合成 `node_id: 0` 的事件（真实节点 id 为正，见宿主
    // `api_plugins::synthetic_events`）。合成事件走的正是这条真实的数据写分支：
    // 不挡住的话，一次自检就在财务页留下一行名为 test 的假节点。
    match serde_json::from_str::<NodeEvent>(input) {
        Ok(NodeEvent::NodeAdded { node_id, name, created_at }) if node_id > 0 => {
            // 身份相同 = 这台机器已经建过记录了（事件重投或迟到），什么都不做，
            // 否则会把操作员刚填好的价格抹回空白；身份不同（或记录缺失）才写空白
            // 记录——那时已有记录只可能属于被删掉的旧机器（id 被复用）。
            let existing = load_node(node_id).and_then(|n| n.host_created_at);
            if existing != created_at {
                save_node(node_id, &blank_node(name, created_at));
            }
        }
        Ok(NodeEvent::NodeDeleted { node_id, created_at }) if node_id > 0 => {
            // 事件异步派发，可能与复用同一 id 的 node_added 乱序；身份不符就当
            // 没发生过，否则会误删新节点的记录。
            let stored = load_node(node_id).and_then(|n| n.host_created_at);
            if stored.is_some() && created_at == stored {
                data_delete(&node_key(node_id));
            }
        }
        // 认不出的载荷、以及 node_id 不合法的合成事件：一条都不写。
        _ => {}
    }
}

/// 把导出拿到的入参字节读成字符串；越界或空入参给空串（两个调用方对空串都是
/// 无事可做）。
fn read_input(ptr: i32, len: i32) -> String {
    if ptr > 0 && len > 0 {
        bytes_to_string(unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) })
    } else {
        String::new()
    }
}

/// 每小时 tick。
#[no_mangle]
pub extern "C" fn on_tick() -> i32 {
    tick();
    0
}

/// 渲染面板页面。打开页面是唯一允许"顺手拉一次汇率"的入口（KTD3）。
#[no_mangle]
pub extern "C" fn render_page(_ptr: i32, _len: i32) -> i32 {
    respond(&build_page(true))
}

/// 处理页面交互。
#[no_mangle]
pub extern "C" fn on_action(ptr: i32, len: i32) -> i32 {
    respond(&handle_action(&read_input(ptr, len)))
}

/// 把一段 JSON 经 host_resp_alloc 的缓冲写回宿主，返回字节数。
fn respond(value: &Value) -> i32 {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let cap = bytes.len() as i32;
    let ptr = unsafe { host_resp_alloc(cap) };
    if ptr <= 0 {
        log(3, "finance-stats: resp_alloc 失败");
        return -1;
    }
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    cap
}
