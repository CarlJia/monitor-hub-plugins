//! 真宿主契约测试:把刚 `./build.sh` 出来的 `plugin.wasm` 用 **monitor 的真实
//! host**(`monitor-plugin-contract`)instantiate,再驱动它。桩烟测用的是仓内
//! 手写宿主,测不出「宿主函数 import 签名漂移」;这个测试会。
//!
//! release.yml 在 `./build.sh` 之后、`gh release create` 之前跑它。
//! 本地跑:先 `./build.sh`,再 `cargo test --test contract`。

use monitor_plugin_contract::{linker, ContractState, MockHttp};

const PLUGIN_ID: &str = "io.github.monitor.finance-stats";

#[test]
fn instantiates_and_runs_against_the_real_host() {
    let engine = wasmtime::Engine::default();
    let wasm = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.wasm"))
        .expect("plugin.wasm 缺失——先跑 ./build.sh");
    let module = wasmtime::Module::new(&engine, &wasm).expect("plugin.wasm 必须能编译");

    // 汇率接口给一个 200 的假响应,on_tick 的刷新步骤能走完。
    let state = ContractState::for_test(PLUGIN_ID)
        .with_http(Box::new(MockHttp::respond_with(200, b"{\"rates\":{\"CNY\":7.2}}")));
    let mut store = wasmtime::Store::new(&engine, state);
    let instance = linker::<ContractState>(&engine)
        .expect("host linker 构建")
        .instantiate(&mut store, &module)
        .expect("真实宿主的 import 签名必须与本插件一致");

    // 真宿主下导出契约:on_event(本插件不订阅事件,no-op)+ on_tick(它的真实工作面)。
    let on_event = instance.get_typed_func::<(i32, i32), i32>(&mut store, "on_event").unwrap();
    assert_eq!(on_event.call(&mut store, (0, 0)).unwrap(), 0);

    let on_tick = instance.get_typed_func::<(), i32>(&mut store, "on_tick").unwrap();
    on_tick.call(&mut store, ()).expect("on_tick 在真实宿主下不得 trap");
}
