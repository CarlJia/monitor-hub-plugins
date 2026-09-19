#![cfg(feature = "contract")]
//! 真宿主契约测试:把刚 `./build.sh` 出来的 `plugin.wasm` 用 **monitor 的真实
//! host**(`monitor-plugin-contract`)instantiate,再驱动它。桩烟测用的是仓内
//! 手写宿主,测不出「宿主函数 import 签名漂移」;这个测试会。
//!
//! release.yml 在 `./build.sh` 之后、`gh release create` 之前跑它。
//! 本地跑:先 `./build.sh`,再 `cargo test --features contract --test contract`。

use monitor_plugin_contract::{
    constants::{DEFAULT_FUEL_LIMIT, DEFAULT_HOOK_FUEL_LIMIT},
    linker, ContractState, MockHttp,
};

const PLUGIN_ID: &str = "io.github.monitor.finance-stats";

/// plugin.toml 的 plugin_id(与常量比对:契约替身用它建 kv 命名空间,对不上会
/// 在错误的命名空间上跑而假绿——assert 而非靠人记)。
fn manifest_plugin_id() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"))
        .expect("plugin.toml")
        .lines()
        .find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == "plugin_id").map(|(_, v)| v))
        .and_then(|v| v.split('"').nth(1))
        .expect("plugin.toml 里应有 plugin_id")
        .to_owned()
}

/// plugin.toml 的 abi_version(与拉到的契约 crate 的 ABI_VERSION 比对:宿主面变了
/// 却没 bump / 没打新 tag 导致拉到旧契约时,门会红而不是验错对象)。
fn manifest_abi_version() -> i64 {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"))
        .expect("plugin.toml")
        .lines()
        .find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == "abi_version").map(|(_, v)| v))
        .and_then(|v| v.trim().parse().ok())
        .expect("plugin.toml 里应有数字 abi_version")
}

#[test]
fn instantiates_and_runs_against_the_real_host() {
    assert_eq!(manifest_plugin_id(), PLUGIN_ID, "PLUGIN_ID 必须与 plugin.toml 的 plugin_id 一致");
    assert_eq!(
        monitor_plugin_contract::ABI_VERSION,
        manifest_abi_version(),
        "拉到的契约 ABI 必须等于本插件 plugin.toml 声明的 abi_version"
    );

    // 与真宿主同配置:开 fuel 计量(不开则 set_fuel 静默无效,燃料耗尽路径不复现)。
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let wasm = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.wasm"))
        .expect("plugin.wasm 缺失——先跑 ./build.sh");
    let module = wasmtime::Module::new(&engine, &wasm).expect("plugin.wasm 必须能编译");

    // 汇率接口给一个 200 的假响应,on_tick 的刷新步骤能走完。
    let state = ContractState::for_test(PLUGIN_ID)
        .with_http(Box::new(MockHttp::respond_with(200, b"{\"rates\":{\"CNY\":7.2}}")));
    let mut store = wasmtime::Store::new(&engine, state);
    // fuel 必须在任何 wasm 执行(含 instantiate 的 start/data 段)之前就位。
    store.set_fuel(DEFAULT_HOOK_FUEL_LIMIT).unwrap();
    let instance = linker::<ContractState>(&engine)
        .expect("host linker 构建")
        .instantiate(&mut store, &module)
        .expect("真实宿主的 import 签名必须与本插件一致");

    // 真宿主下导出契约:on_event(不订阅事件,no-op)用派发档预算。
    let on_event = instance.get_typed_func::<(i32, i32), i32>(&mut store, "on_event").unwrap();
    store.set_fuel(DEFAULT_FUEL_LIMIT).unwrap();
    assert_eq!(on_event.call(&mut store, (0, 0)).unwrap(), 0);

    // on_tick(它的真实工作面)用数据面钩子那档预算,与生产一致。
    let on_tick = instance.get_typed_func::<(), i32>(&mut store, "on_tick").unwrap();
    store.set_fuel(DEFAULT_HOOK_FUEL_LIMIT).unwrap();
    on_tick.call(&mut store, ()).expect("on_tick 在真实宿主下不得 trap");
}
