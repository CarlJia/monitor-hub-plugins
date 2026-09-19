# monitor-hub-plugins — AI 协作说明

## 这是什么仓

本仓是 [monitor-hub](https://github.com/CarlJia/monitor) 的**插件子仓**：所有 WASM 插件源（`tg-notify`、`finance-stats`）与脚手架 skill（`.claude/skills/create-plugin/`）。monitor 仓只保留宿主实现与 ABI 文档。

## ABI 真相源（重要）

ABI 字段表、host_funcs 实现、manifest 校验逻辑**不在本仓**——它们在 monitor 仓：

- **ABI 文档与字段表**：monitor 仓 `README.md` 的「插件开发」章节
- **host 函数实现**:`monitor/src/plugin/host_funcs.rs` + 契约 crate `monitor/crates/monitor-plugin-contract/`(13 个宿主函数的**唯一实现**在契约 crate;`host_funcs.rs` 只留 SSRF 网段判定与真 reqwest 调用)
- **manifest 校验**：`monitor/src/plugin/manifest.rs`

改 ABI 必须先在 monitor 仓改 host_funcs.rs / manifest.rs(契约 crate 随之更新,版本同步由 monitor CI 守)。本仓的 ABI 兼容性有**三道门**,详见下面「跨仓信号」:真宿主契约(release.yml 的发布门 + `ci.yml` 的 PR 门,**主防线**)、`ci.yml` 的 abi-gate(声明级整数门)、`abi-check.yml`(定时轮询)。

## 工作流

- **新建插件**：用 `.claude/skills/create-plugin/` skill（触发词："create a new plugin" / "scaffold a plugin" / "写个新插件"）。它会按 plugin.toml schema 与 Cargo.toml 模板给出骨架。
- **本地开发**：`cd <plugin>/` → `cargo test`（跑 wasmtime 桩宿主 smoke test）→ `./build.sh` 产 `plugin.tar.gz`
- **发布**：打 tag `tg-notify-vX.Y.Z` 或 `finance-stats-vX.Y.Z`，release.yml 自动产出 GitHub Release

## 版本约定

- 当前所有插件从 `1.0.0` 起（拆仓点 fresh-start）
- Patch = bug fix；Minor = 新增功能（保持 ABI v2 兼容）；Major = 破坏 ABI 兼容（需 monitor 先 bump）

## 跨仓信号:三道门

1. **真宿主契约(`release.yml` + `ci.yml` 的 contract 步)—— 主防线**。按本插件 `plugin.toml` 的 `abi_version` 拉到对应主版本的 `monitor-plugin-contract` crate,用 **monitor 的真实宿主** instantiate 本插件的 `plugin.wasm` 并驱动它(`tests/contract.rs`)。宿主函数 import 签名或导出契约漂移 → `instantiate` 失败 → 红。这是唯一能发现「宿主签名变了但 `abi_version` 整数没变」的门。`release.yml` 在 `./build.sh` 之后跑(发布门);`ci.yml` 在每个 PR 与 main 的 push 上跑**同一条命令**(PR 门),所以漂移在 PR 上就红,不用等 release。
   注意 `tests/contract.rs` 首行是 `#![cfg(feature = "contract")]`,**裸 `cargo test` 会把整个文件编译成空**——必须带 `--features contract` 才真跑。
2. **`ci.yml` 的 abi-gate(每次 push/PR)** —— 声明级:对照 monitor 当前 `ABI_VERSION` 校验各 `plugin.toml` 的整数。
3. **`abi-check.yml`(定时轮询 monitor HEAD)** —— 声明级 + 桩测试:整数匹配 + 桩烟测。

契约 crate 的版本取自 monitor 仓的 tag `monitor-plugin-contract-v<abi>.*`(取 `sort -V` 最大者),插件仓按 tag 拉。**tag 指向的提交内容才是被消费的东西**:monitor 的 `publish-contract.yml` 现在会校验「tag 上的 crate 与 main 上的逐字节一致」,在 tag 是过期提交时当场红——2026-09-19 就出过 `v2.0.0` 打在过期提交上、两个插件 release 全部编译失败的事。

## 仍不由任何门覆盖的

真宿主契约测的是**签名/导出契约 + 本次驱动的那条路径**;下面这些仍只在生产暴露:

- 调 monitor 运行时预算(如 `DEFAULT_HOOK_FUEL_LIMIT`)——不在 ABI 里;`tests/smoke.rs` 的 `PROD_HOOK_FUEL` 是手抄常量(桩烟测是 fast feedback,非契约)。
- 契约测试只驱动一条事件 / 一个 hook;插件的其余分支靠 `tests/smoke.rs` + 手测。
- **契约 crate 改了、但没在 monitor 打新 tag** —— 三个门都按 `abi_version` 去取现成的 `monitor-plugin-contract-v<abi>.*` tag,而 `publish-contract.yml` 只在 tag push 时触发。所以「同 ABI 的契约修复合进 main 却没打 tag」不被任何门发现,插件会对着旧 crate 发版。反向的错(tag 打在过期提交上)已由 monitor 侧的内容一致性校验挡住。

写新插件时要过 release 契约门:`plugin.toml` 的 `plugin_id` 必须与 `tests/contract.rs` 里 `ContractState::for_test(...)` 传的 id 逐字一致(契约替身用它建 kv 命名空间)。
