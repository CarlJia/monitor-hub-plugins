# monitor-hub-plugins — AI 协作说明

## 这是什么仓

本仓是 [monitor-hub](https://github.com/CarlJia/monitor) 的**插件子仓**：所有 WASM 插件源（`tg-notify`、`finance-stats`）与脚手架 skill（`.claude/skills/create-plugin/`）。monitor 仓只保留宿主实现与 ABI 文档。

## ABI 真相源（重要）

ABI 字段表、host_funcs 实现、manifest 校验逻辑**不在本仓**——它们在 monitor 仓：

- **ABI 文档与字段表**：monitor 仓 `README.md` 的「插件开发」章节
- **host 函数实现**：`monitor/src/plugin/host_funcs.rs`（14 个函数）
- **manifest 校验**：`monitor/src/plugin/manifest.rs`

改 ABI 必须先在 monitor 仓改 host_funcs.rs / manifest.rs；本仓的 `abi-check.yml` 会在 monitor 主分支 push 后自动验证本仓插件是否与新 ABI 兼容。

## 工作流

- **新建插件**：用 `.claude/skills/create-plugin/` skill（触发词："create a new plugin" / "scaffold a plugin" / "写个新插件"）。它会按 plugin.toml schema 与 Cargo.toml 模板给出骨架。
- **本地开发**：`cd <plugin>/` → `cargo test`（跑 wasmtime 桩宿主 smoke test）→ `./build.sh` 产 `plugin.tar.gz`
- **发布**：打 tag `tg-notify-vX.Y.Z` 或 `finance-stats-vX.Y.Z`，release.yml 自动产出 GitHub Release

## 版本约定

- 当前所有插件从 `1.0.0` 起（拆仓点 fresh-start）
- Patch = bug fix；Minor = 新增功能（保持 ABI v2 兼容）；Major = 破坏 ABI 兼容（需 monitor 先 bump）

## 跨仓信号

`abi-check.yml` 的 workflow summary 是 plugin author 看到的 ABI 兼容性状态；monitor ABI 升时这里会自动跑验证。
