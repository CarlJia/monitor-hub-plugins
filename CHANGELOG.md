# Changelog

历史 tag 与 commit 内版本号的对照放在文末「历史发布 Tag / 版本错位」一节。版本发布明细以 [GitHub Releases](https://github.com/CarlJia/monitor-hub-plugins/releases) 为准。

本仓是 [monitor](https://github.com/CarlJia/monitor) 的插件子仓，所有插件从 `1.0.0` 起（拆仓点 fresh-start）；版本约定见根目录 `CLAUDE.md`（Patch = bug fix，Minor = 新增功能保持 ABI v2 兼容，Major = 破坏 ABI 兼容）。

## 历史发布 Tag / 版本错位（只读，不重建）

下列 tag 在发布时**没有同步更新** `Cargo.toml` 里的版本号。重建需要 `push --force`，会影响远端已引用的 tag；本节只标记、保留 tag 不动。

### 类型 A：发布漏改（`Cargo.toml` 没跟着 tag 升级）

#### finance-stats

| Tag | 指向 commit 的 `version` | 期望 | 状态 |
|-----|--------------------------|------|------|
| `finance-stats-v1.0.0` | `1.0.0` | `1.0.0` | ✓ |
| `finance-stats-v1.0.1` | `1.1.0` | `1.0.1` | 漏改（同步节点增删事件时 bump 到 1.1.0，但 tag 用了 `v1.0.1`） |
| `finance-stats-v1.0.2` | `1.1.0` | `1.0.2` | 漏改（同上，merge commit 也未跟改） |
| `finance-stats-v1.2.0` | `1.2.0` | `1.2.0` | ✓ |

#### tg-notify

| Tag | 指向 commit 的 `version` | 期望 | 状态 |
|-----|--------------------------|------|------|
| `tg-notify-v1.0.0` | `1.0.0` | `1.0.0` | ✓ |
| `tg-notify-v1.0.2` | `1.0.0` | `1.0.2` | 漏改（merge commit 时 Cargo.toml 没跟着 tag 升级） |

当前 main 上 `tg-notify` 已 bump 到 `1.0.2`，与 `tg-notify-v1.0.2` tag 名一致；该 tag 指向的 commit 上是历史漏改的 `1.0.0`。

### 重建映射（按 `Cargo.toml` 反查 tag）

- `finance-stats = "1.0.0"` → tag `finance-stats-v1.0.0` ✓
- `finance-stats = "1.1.0"` → 两个 tag 都指向该版本 commit：`finance-stats-v1.0.1` / `finance-stats-v1.0.2`——类型 A
- `finance-stats = "1.2.0"` → tag `finance-stats-v1.2.0` ✓
- `tg-notify = "1.0.0"` → 两个 tag 都指向：`tg-notify-v1.0.0` ✓ 与 `tg-notify-v1.0.2`（类型 A）
- `tg-notify = "1.0.2"` → 当前 main HEAD，tag `tg-notify-v1.0.2` 虽同名但指向历史 commit（类型 A）

如需引用「Cargo.toml 版本号 = tag 名」对应关系，请使用 `finance-stats-v1.0.0` / `finance-stats-v1.2.0` 与 main HEAD 上的 `tg-notify` 1.0.2。
