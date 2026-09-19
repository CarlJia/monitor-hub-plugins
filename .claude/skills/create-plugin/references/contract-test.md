# 契约测试(release 门 + PR 门)

每个插件都带一份 `tests/contract.rs`:发布前与每个 PR(`release.yml` + `ci.yml`)用
**monitor 的真实宿主** `monitor-plugin-contract` instantiate 本插件的 `plugin.wasm`
并驱动一条路径。它测的是**宿主函数 import 签名 / 导出契约**——桩烟测
(`tests/smoke.rs`)测不出这个。

## 两个必备件

1. **`Cargo.toml` 里声明空的 `contract` feature**,并把 `tests/contract.rs` 用
   `#![cfg(feature = "contract")]` 罩住:

   ```toml
   [features]
   contract = []
   ```

   原因:`monitor-plugin-contract` 是**临时加的 dev-dep(不进仓)**,所以 feature
   关掉时整个文件为空、不引用那个缺失的 crate,裸 `cargo test` 照常绿。真跑必须先
   `cargo add` 它再带 `--features contract`——`release.yml`(发布门)与 `ci.yml`
   (PR 门)都这么做。**漏掉 `--features contract` 会编译出 0 个用例并静默通过**;
   这正是它过去只在 release 才暴露的原因。

2. **`tests/contract.rs`**——把下面的模板里 `PLUGIN_ID` 换成你的
   `plugin.toml` 的 `plugin_id`,驱动的事件换成本插件真实订阅的事件(见
   `plugin.toml` 的 `subscribes`):

   ```rust
   #![cfg(feature = "contract")]
   use std::sync::Arc;
   use monitor_plugin_contract::{
       constants::DEFAULT_FUEL_LIMIT, linker, setting_key, ContractState, MockHttp,
   };

   const PLUGIN_ID: &str = "io.github.<user>.<name>";   // = plugin.toml 的 plugin_id

   #[test]
   fn instantiates_and_drives_an_event_against_the_real_host() {
       // 与真宿主同配置:开 fuel 计量(不开则 set_fuel 静默无效)。
       let mut config = wasmtime::Config::new();
       config.consume_fuel(true);
       let engine = wasmtime::Engine::new(&config).unwrap();
       let wasm = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.wasm"))
           .expect("plugin.wasm 缺失——先跑 ./build.sh");
       let module = wasmtime::Module::new(&engine, &wasm).unwrap();

       let http = Arc::new(MockHttp::respond_with(200, b"{\"ok\":true}"));
       let state = ContractState::for_test(PLUGIN_ID).with_http(Box::new(http.clone()));
       // 需要渠道配置的插件:预置 kv(命名空间由替身按 plugin_id 拼)。
       state.kv().set(&setting_key(PLUGIN_ID, "…"), "…");

       let mut store = wasmtime::Store::new(&engine, state);
       store.set_fuel(DEFAULT_FUEL_LIMIT).unwrap();
       let instance = linker::<ContractState>(&engine).unwrap()
           .instantiate(&mut store, &module)
           .expect("真实宿主的 import 签名必须与本插件一致");   // ← 签名漂移在这里失败

       let on_event = instance.get_typed_func::<(i32, i32), i32>(&mut store, "on_event").unwrap();
       let alloc = instance.get_typed_func::<(i32,), i32>(&mut store, "__alloc").unwrap();
       let payload = br#"{"type":"agent_offline",…}"#;   // 本插件订阅的事件
       let ptr = alloc.call(&mut store, (payload.len() as i32,)).unwrap();
       let mem = instance.get_memory(&mut store, "memory").unwrap();
       mem.data_mut(&mut store)[ptr as usize..ptr as usize + payload.len()].copy_from_slice(payload);
       assert_eq!(on_event.call(&mut store, (ptr, payload.len() as i32)).unwrap(), 0);
   }
   ```

## 本地怎么跑

```sh
./build.sh                      # 在插件目录里：产出 plugin.wasm
../scripts/contract-check.sh .  # 仍在插件目录里跑（脚本自己 cd 进目标目录），
                                # 取 monitor 上对应 ABI 的正式 tag、拉那版契约
                                # crate、跑 tests/contract.rs，收尾还原 Cargo.toml
```

tag 规则与命令只在 `scripts/contract-check.sh` 里有一份实现——`ci.yml`(每个 PR)
与 `release.yml`(每次发版)都调它。别把手写命令抄进插件仓:抄出来的那份会漏掉
「只认正式 tag」的过滤(`sort -V` 会把 `-rc1` 排在同版本正式 tag 之后,而 monitor
判失败后不会回滚那个 tag)。

模板里的 `PLUGIN_ID` 与 `plugin.toml` 的 `plugin_id` 必须逐字一致(替身用它建 kv
命名空间);测试里那条 assert 会替你挡住不一致,但变量名别写错。

## 边界

契约门驱动**一条路径**(你写的那条);其余分支仍靠 `tests/smoke.rs` + 手测。宿主
改了函数签名但没 bump `abi_version` 时,`instantiate` 失败即红——这正是它要抓的。
