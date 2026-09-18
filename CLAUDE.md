# monitor-hub-plugins — AI 协作说明

## 这是什么仓

本仓是 [monitor-hub](https://github.com/CarlJia/monitor) 的**插件子仓**：所有 WASM 插件源（`tg-notify`、`finance-stats`）与脚手架 skill（`.claude/skills/create-plugin/`）。monitor 仓只保留宿主实现与 ABI 文档。

## ABI 真相源（重要）

ABI 字段表、host_funcs 实现、manifest 校验逻辑**不在本仓**——它们在 monitor 仓：

- **ABI 文档与字段表**：monitor 仓 `README.md` 的「插件开发」章节
- **host 函数实现**：`monitor/src/plugin/host_funcs.rs`（14 个函数）
- **manifest 校验**：`monitor/src/plugin/manifest.rs`

改 ABI 必须先在 monitor 仓改 host_funcs.rs / manifest.rs。本仓的 ABI 兼容性有两道检查：`ci.yml` 在每次 push/PR 上跑一道便宜的 ABI 门（对照 monitor 当前 `ABI_VERSION` 校验各 `plugin.toml`）；`abi-check.yml` 每小时轮询 monitor 主分支 HEAD，HEAD 未变时跳过，变则读取 `ABI_VERSION` 并重跑契约测试。注意 `workflow_run` 不能跨仓触发，所以是轮询而非 push 即时。

## 工作流

- **新建插件**：用 `.claude/skills/create-plugin/` skill（触发词："create a new plugin" / "scaffold a plugin" / "写个新插件"）。它会按 plugin.toml schema 与 Cargo.toml 模板给出骨架。
- **本地开发**：`cd <plugin>/` → `cargo test`（跑 wasmtime 桩宿主 smoke test）→ `./build.sh` 产 `plugin.tar.gz`
- **发布**：打 tag `tg-notify-vX.Y.Z` 或 `finance-stats-vX.Y.Z`，release.yml 自动产出 GitHub Release

## 版本约定

- 当前所有插件从 `1.0.0` 起（拆仓点 fresh-start）
- Patch = bug fix；Minor = 新增功能（保持 ABI v2 兼容）；Major = 破坏 ABI 兼容（需 monitor 先 bump）

## 跨仓信号与它的边界（重要）

两道门校验的都是**声明的整数 `abi_version`**,契约测试跑的是**仓内 wasmtime 桩宿主**（`tests/smoke.rs`）——**不是** monitor 的真实宿主。因此下面这些**不会被任何门发现**,只在生产实例化/运行时炸:

- 改 `host_funcs.rs` 的函数签名/语义但**没 bump** `ABI_VERSION`（整数没变 → 全绿；桩没跟着改 → 桩测试也绿）
- 调 monitor 的运行时预算（如 `DEFAULT_HOOK_FUEL_LIMIT`）——不属于 ABI，无门反应；smoke 里的 `PROD_HOOK_FUEL` 是**手抄**常量
- quota / SSRF / deadline / record-cap 等宿主行为——桩不强制

跨仓 ABI 一致性目前**靠人工纪律**（bump 整数、保持桩与真宿主同步）。要根治需在 monitor 侧加「host import 面 fingerprint 测试」或「真宿主契约 harness」,那是独立于本次拆仓的工作。
