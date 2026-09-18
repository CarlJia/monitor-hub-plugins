# Page protocol — JSON UI blocks

`render_page` and `on_action` both write JSON to a buffer via `host_resp_alloc` and return the byte count. The panel parses this JSON and renders blocks. Strings live in the plugin (the panel doesn't know business semantics); values and labels come from one source of truth (e.g. `CURRENCIES`, `CYCLE_MONTHS`).

## Response shape

```json
{
  "title": "财务统计",
  "blocks": [
    { "type": "notice",  ... },
    { "type": "stat",    ... },
    { "type": "table",   ... },
    { "type": "form",    ... }
  ],
  "toast": { "kind": "success|warning|error", "text": "..." }
}
```

`toast` is optional, attached to action responses to show one transient message (the panel pops it once, then drops it).

## Block types

### `notice` — text banner

```json
{ "type": "notice", "kind": "warning", "text": "汇率不可用——尚未成功拉取过汇率，统计暂缺" }
```

`kind` is optional; `info` is default, `warning` for yellow.

### `stat` — KPI / selector row

```json
{
  "type": "stat",
  "items": [
    { "label": "年化续费总成本", "value": "¥1,234.56" },
    { "label": "展示币种", "select": {
        "value": "CNY",
        "options": [{"value": "CNY", "label": "¥ CNY"}, {"value": "USD", "label": "$ USD"}],
        "action": "set_currency"
    }}
  ]
}
```

An `item` is either `{label, value}` (read-only text) or `{label, select: {value, options, action}}` (interactive dropdown). `action` is the action name the panel sends to `on_action` when the user changes the value.

### `table` — read-only table

```json
{
  "type": "table",
  "title": "7 天内到期（3）",
  "columns": ["节点", "到期日", "剩余天数", "备注"],
  "rows": [
    ["node-a", "2026-09-20", 2, ""],
    ["node-b", "2026-09-22", 4, "免费"]
  ]
}
```

`rows` is an array of arrays (each inner array = one row, position-aligned with `columns`).

### `form` — editable table

```json
{
  "type": "form",
  "title": "节点财务数据",
  "action": "save_node",
  "fields": [
    { "name": "name",          "label": "节点名", "type": "text" },
    { "name": "price",         "label": "价格",   "type": "money", "prefix_key": "price_symbol" },
    { "name": "currency",      "label": "币种",   "type": "select", "options": [{"value": "CNY", "label": "¥ CNY"}] },
    { "name": "billing_cycle", "label": "计费周期", "type": "select", "options": [{"value": "yearly", "label": "年付"}] },
    { "name": "expires_at",    "label": "到期日", "type": "date" }
  ],
  "rows": [
    { "id": 1, "name": "node-a", "price": 12.5, "price_symbol": "¥", "currency": "CNY", "billing_cycle": "yearly", "expires_at": "2026-12-01" }
  ]
}
```

`action` is what the panel sends to `on_action` when the user submits. `rows` is an array of objects keyed by `fields[].name`. `id` is conventional — the plugin decides which field is the row key.

#### Field types

| Type | Renders as | Notes |
|------|-------------|-------|
| `text` | text input | string value |
| `date` | date picker | `YYYY-MM-DD`; send `""` from UI to clear |
| `select` | dropdown | needs `options: [{value, label}]`; submits `value` |
| `money` | numeric input, right-aligned, 2-decimal | needs `prefix_key` (a column name on the same row whose value is the prefix string — symbol or currency code) |

`money` doesn't recognize currency codes — that's intentional. The plugin computes the prefix (e.g. `"¥"` from `CURRENCIES[code].symbol`) and emits it on each row as a sibling column; the panel picks it up via `prefix_key` and renders `¥12.50`. This keeps the panel generic.

## on_action input

The panel sends a JSON object whose `action` field matches an `action` declared in your `select` or `form`:

```json
{ "action": "set_currency", "value": "USD" }
{ "action": "save_node",    "id": 1, "name": "node-a", "price": 14.0, ... }
```

Unknown actions fall through to a default branch (typically return the page unchanged). Return a fresh `build_page(...)` + optional `toast`.

## Anti-patterns to avoid

- **Don't embed business labels in field labels.** Labels come from the same table as the value (`CYCLE_MONTHS` derives both). Picking `value` from the dropdown and looking up `label` in a separate map drifts over time.
- **Don't make the panel render business strings.** The panel knows `notice`/`stat`/`table`/`form` shapes and that's it. Currencies, cycles, statuses — all defined in the plugin.
- **Don't hide a failed refresh behind an unchanged UI.** If `refresh_fx` fails, attach a `toast` with the reason (and a `notice` block on the page itself) so the operator sees why numbers didn't move.