# monitor-hub-plugins

monitor-hub 的 WASM 插件仓库（ABI v2）。

## 这是什么

[monitor-hub](https://github.com/CarlJia/monitor) 是一个 hub 形态的监控/节点管理系统。本仓是其**插件子仓**——所有 WASM 插件源与插件脚手架 skill 独立在这里维护，monitor 仓只保留宿主实现与 ABI 文档。

## ABI 真相源

**ABI 文档与宿主实现在 monitor 仓**，本仓不复制：

- ABI 字段表 / 插件开发指南：见 monitor 仓 `README.md` 的「插件开发」章节
- 宿主函数实现：`monitor/src/plugin/host_funcs.rs`（14 个宿主函数）
- Manifest 校验：`monitor/src/plugin/manifest.rs`

拆仓后 ABI 文档与 host 实现留在 monitor；插件仓通过反向链接引用，避免双源真相漂移。

`abi-check.yml` 在 monitor 主分支 push 后跨仓跑契约 smoke test，确保插件源码与当前 monitor ABI 一致。

## 仓库结构

```
monitor-hub-plugins/
├── tg-notify/                  ← 通知插件（订阅 agent_online/offline、plugin_expiry_soon）
├── finance-stats/              ← 财务统计插件（tick + page + cleanup + emit + data）
├── .claude/skills/
│   └── create-plugin/          ← 脚手架 skill（Scaffold a new WASM plugin）
└── .github/workflows/
    ├── ci.yml                  ← wasm32 build + cargo test（push / PR）
    ├── abi-check.yml           ← workflow_run 跨仓 ABI 契约测试
    └── release.yml             ← per-plugin tag → GitHub Release
```

## 贡献流程

1. 装 wasm32 目标：`rustup target add wasm32-unknown-unknown`
2. 用 create-plugin skill 起新插件骨架（`.claude/skills/create-plugin/SKILL.md`）
3. 在你的插件子目录下开发、跑 `./build.sh`、跑 `cargo test`
4. 提交 PR；ci.yml 会跑 wasm32 build + smoke test
5. PR 合并后打 tag（`tg-notify-vX.Y.Z` 或 `finance-stats-vX.Y.Z`），release.yml 自动产出 `plugin.tar.gz` 并 attach 到 GitHub Release
6. Operator 从 GitHub Release 下载 tarball 经管理面板「插件」页面上传

## 版本语义

- 拆仓点 fresh-start：版本从 `1.0.0` 起
- Patch：bug fix；Minor：新增功能（保持 ABI v2 兼容）；Major：破坏 ABI 兼容（需 monitor 先 bump ABI）

## License

MIT
