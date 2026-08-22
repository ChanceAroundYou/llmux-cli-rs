# 聚合别名（Aggregate Alias）设计

日期：2026-08-22
状态：设计已确认，待写实现计划

## 目标

LLMux 现有别名是 `alias → (provider_id, target_model, account_ids[], preferred_account_id)`——一个别名对应**一个**目标模型，`DispatchRouter` 只在该模型的多个账户间做粘性故障切换。

本特性新增一种**聚合别名**：一个别名后面挂一串**有序候选**，每个候选是独立的 `(account_id, model)` 对（账户是唯一键，与厂商无关）：

```
test:
  1. account=deepseek  model=deepseek-v4-pro     ← 默认
  2. account=command   model=deepseek-v4-flash
  3. account=go1       model=hy3
```

请求命中 `test` 时按固定顺序尝试，失败降级到下一候选；后台每分钟探测默认候选的连通性，健康时保持在默认，故障时提前降级、恢复后自动升回，避免"每个请求都先吃一次失败"。

仅覆盖 **OpenAI + Anthropic** 协议；Gemini 不做聚合（日常少走）。

## 范围边界

- **做**：`aggregate_aliases` 独立建表；`resolve_aggregate` 解析；OpenAI / Anthropic 两个 handler 的聚合派发；后台探测（降级/恢复）；`/api/aggregate-aliases` CRUD；UI 模型页第三个按钮（聚合别名）；**移除**自定义别名整套功能，聚合别名按钮替换其位置。
- **不做**：Gemini 聚合；改动普通别名语义；改动 `/api/models/test`（测试全部按钮与后台探测都复用它）；改动 API Key 白名单逻辑（聚合别名名就是一个普通 model id）。

## 数据模型

### 迁移 `0007_add_aggregate_aliases.sql`

```sql
CREATE TABLE IF NOT EXISTS aggregate_aliases (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  alias         TEXT NOT NULL UNIQUE,
  candidates    TEXT NOT NULL,               -- JSON 数组，按优先级
  interval_secs INTEGER NOT NULL DEFAULT 60, -- 后台探测间隔
  created_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_aggregate_aliases_alias ON aggregate_aliases(alias);
```

`candidates` 形如：

```json
[
  {"account_id": 1, "model": "deepseek-v4-pro"},
  {"account_id": 5, "model": "deepseek-v4-flash"},
  {"account_id": 7, "model": "hy3"}
]
```

- **数组顺序即故障切换顺序，`candidates[0]` 即默认模型。** 不设独立 `default_index` 字段——改默认/改顺序就是重排列表，避免两处状态打架。
- 运行时"当前降级到第几个"是**内存态**（`AggregateRouter`），不落库。进程重启即回到默认。
- 候选列表在 `set_model_alias` 这类写入时整体替换（`INSERT OR REPLACE`），天然原子。
- **写入（upsert）或删除聚合别名时，清除该别名的内存态 entry**（`aggregate_router.entries.remove(alias)`），下次请求重新初始化为默认 `current_index = 0`。避免旧的降级下标指向新候选列表的错误位置。

### `AppState` 扩展

```rust
pub struct AggregateRouter {
    // alias -> Current state (index into candidates + probe backoff)
    entries: HashMap<String, AggregateEntry>,
}
pub struct AggregateEntry {
    current_index: usize,          // 当前应使用的候选下标
    probe_backoff_secs: u64,       // 探测失败指数退避
    last_probe: Instant,
    last_status: Option<bool>,     // 最近一次探测是否连通（供 UI 显示）
}
```

`AppState` 新增字段 `aggregate_router: Arc<Mutex<AggregateRouter>>`，`AppState` 已是 `#[derive(Clone)]`，Arc 字段自动满足。

> 注：`DispatchRouter`（现有）与 `AggregateRouter`（新）职责不同——前者在**同一模型的多个账户**间粘性切换；后者在**不同模型的多个候选**间切换，且每步存在一个独立模型名。二者并存，不合并。

## 服务端解析

在 `llmux-core/src/dispatcher.rs`（或新模块 `aggregate.rs`）新增：

```rust
pub struct AggregateResolution {
    pub alias: String,
    pub candidates: Vec<AggregateCandidate>,
    pub current_index: usize,   // 从 AggregateRouter 读出的起始下标
}
pub struct AggregateCandidate {
    pub account_id: i64,
    pub model: String,
}

pub async fn resolve_aggregate(pool, model_name, router: &AggregateRouter)
    -> anyhow::Result<Option<AggregateResolution>>
```

- 先查现有 `model_aliases`，命中普通别名 → 返回 `None`（走原逻辑）；未命中再查 `aggregate_aliases`，命中 → 返回 `Some(...)`；都没命中 → 走 `resolve_model_by_prefix` 兜底（原逻辑）。
- `current_index` 从 `AggregateRouter.entries[alias]` 读出；不含该 entry 时 `= 0`（默认）。

`resolve_model_cached` 是现有三个 handler 的统一入口，但它返回单目标 `ModelResolution`。聚合命中时不用它，而是让 handler 顶部短路到聚合派发：

```
if let Some(agg) = resolve_aggregate(...).await? { return dispatch_aggregate_openai(...).await; }
```

## 服务端派发

### OpenAI —— `dispatch_aggregate_openai`

新函数，放 `crates/llmux-server/src/routes/v1/openai.rs`。签名与 `openai_dispatch` 对齐，仅把"单目标"换成"按候选循环"：

```rust
async fn dispatch_aggregate_openai(
    state, auth, uri, headers, body, endpoint,
    agg: AggregateResolution,
) -> Response
```

核心差异：每跳一个候选，`body["model"]` 用**该候选的真实模型名**覆盖，然后构造请求。伪代码：

```
for i in agg.current_index..agg.candidates.len() {
    let cand = &agg.candidates[i];
    let account = get_account_by_id(pool, cand.account_id, master_key)?;
    if account.is_none() or inactive { record degrade; continue; }
    let mut patched = body.clone();
    patched["model"] = cand.model;                       // 关键：每步换模型名
    ...build ProviderRequest (base_url per provider_id)...
    let resp = execute_provider_request(req).await;
    if ok {
        router.record_success(alias, i);                 // current_index 记为 i
        ...usage logging / streaming passthrough (复用现有原语)...
        return resp;
    }
    router.record_failure(alias, i);                     // current_index = i+1 (降级)
    continue;
}
return 502 all exhausted; 且 record_failure 到最后一位
```

`chat_completions` / `responses` 两个入口在现有 `openai_dispatch` 体首行加短路：

```rust
if let Some(acc) = state.resolve_aggregate_cached(&model_name)? {
    return dispatch_aggregate_openai(state, auth, uri, headers, body, endpoint, acc).await;
}
```

### Anthropic —— `dispatch_aggregate_anthropic`

新函数，放 `crates/llmux-server/src/routes/v1/anthropic.rs`。逻辑同 OpenAI 版，但请求构造/响应解析按 Anthropic 协议（`anthropic_base_url` 原生透传，或 `base_url` 下 Anthropic→OpenAI 转换，与现有 `messages` 一致）。`messages` handler 同样在顶部加短路。

### 公共原语

- 账户加载：复用 `get_accounts_by_ids` / 新增 `get_account_by_id`（`account_id` 是唯一键，单账户更直接）。
- 记账：`spawn_log_usage`（现有）。
- UI/TUI 事件：`send_tui_request`、`TuiEvent::Dispatch/Retry`（现有）。
- Streaming 透传：`openai_streaming_passthrough`、`anthropic_streaming_passthrough`、`anthropic_to_openai_streaming`（现有）。

### 降级/恢复状态机（`AggregateRouter`）

两条独立路径，共用同一个 `current_index`，但推进规则不同：

**请求失败路径**（每候选真实请求失败时触发，向下游滑）：

- `record_success(alias, used_index)`：`current_index = used_index`（粘住当前成功候选，保持 prompt cache 热度）；`last_status=Some(true)`。
- `record_failure(alias, failed_index)`：`current_index = failed_index + 1`（移到下一候选）；`last_status=Some(false)`；若已是末尾则停在 `len-1`（仍用最后候选，不自愈成 0）。请求侧可以在一次请求内连续滑到末尾（`for i in current_index..len`）。

**探测路径**（后台周期探测默认候选，只在"默认"与"第一个降级位"间切换）：

- 默认健康且当前在默认 → `current_index = 0`。
- 默认故障且当前就在默认（`current_index == 0`）→ `current_index = 1`（让请求跳过死掉的默认）。
- 默认故障但已降级（`current_index >= 1`）→ `current_index` 保持不动（默认故障已反映在当前用法上，不往深推——深层的切换全权交给请求失败路径）。
- 默认恢复（`record_probe_success`）→ `current_index = 0`，`last_status=Some(true)`。
- `record_probe_failure` → `last_status=Some(false)`。

**退避**：初始 60s；`record_probe_failure` 时 `probe_backoff_secs = min(secs*2, 600)`；`record_probe_success` / `record_success` 都重置回 60。

> 简言之：**探测只防"默认死了还往里打"（一键降级到 1 并随时升回 0）；真正的逐级切换由请求失败驱动。** 二者不冲突。

## 后台探测（每分钟降级/恢复）

`tokio::spawn` 后台循环，挂在 `main.rs`（与测试队列同层）：

```
loop {
    // 对每个聚合别名:
    for (alias, entry) in router.snapshot() {
        let default_cand = &candidates[0];                 // 恒探测默认候选
        // 构造轻量真实请求:
        if account.protocol == anthropic {
            POST /v1/messages  { model: default_cand.model, max_tokens: 1, messages:[{role:"user",content:"ping"}] }
        } else {
            POST /chat/completions { model: default_cand.model, max_tokens: 1, messages:[...] }
        }
        timeout 10s
        if ok => router.record_probe_success(alias);   // current_index -> 0 (若曾降级则升回)
        else  => router.record_probe_failure(alias);   // 降级 index+1, 指数退避
    }
    sleep(interval_secs)   // 默认 60s
}
```

- **探测对象恒为默认候选 `candidates[0]`**（对齐需求："定时测试默认模型的连通性"）。默认健康时 `current_index` 保持 0；默认故障时降级到候选 1（让请求跳过死掉的默认，不再每个请求吃一次失败）；默认恢复时升回 0。
- `record_probe_failure`：若 `current_index == 0`（当前就在默认）→ `current_index = 1`；若已降级（`current_index >= 1`）→ 保持不动（默认故障已反映在当前用法上，不再往深推）。`last_status=Some(false)`，退避翻倍。
- `record_probe_success`：`current_index = 0`（升回默认），`last_status=Some(true)`，退避重置为初始 60。
- 退避：初始 60，失败翻倍封顶 600；`record_probe_success` 与请求侧 `record_success` 都重置退避到 60。

> 设计决策：后台探测与请求时降级共用同一套 `AggregateRouter` 状态，避免两个状态机打架。请求时若在降级区间命中成功候选，同样会把 `current_index` 记为成功位；探测成功则强制拉回默认。

## HTTP API

新路由（`crates/llmux-server/src/routes/models/aggregate.rs`），注册进 `core_router()`：

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/aggregate-aliases` | 列表，每项带 `current_index` + 候选数组 + 每候选最新探测状态（含 `last_status`, latency） |
| `POST` | `/api/aggregate-aliases` | 创建/替换（`upsert` by alias），body 含 `{alias, candidates:[{account_id,model},...], interval_secs}` |
| `DELETE` | `/api/aggregate-aliases/:id` | 删除，清内存态 entry |

写入时校验：`alias` 非空、`candidates` 至少 1 项、每个 `account_id` 指向存在的启用账户、`model` 非空。非法 → 400。

`/v1/models` 列表：聚合别名作为普通 model id 透出（`context_length` 取默认候选 `candidates[0].model` 的窗口，复用 `resolve_alias_context` 逻辑，但传入默认候选的模型名）。API Key 白名单按聚合别名名判断，不变。

## UI（模型页）

顶部按钮从

```
[测试全部] [刷新] [新增别名] [自定义别名]
```

改为

```
[测试全部] [刷新] [新增别名] [聚合别名]     ← 聚合别名占原"自定义别名"位置
```

### 移除（自定义别名整套，纯前端）

- `CustomAliasModal` 组件。
- state：`customForm`、`isCustomModalOpen`、`isVerifying`、`verifyResult`。
- handler：`handleVerify`、`handleCustomAddAlias`。
- i18n key：`models.customAlias*`、`models.verify*`、`models.selectAccount*`、`models.manualModel*`。
- 保留：`Plus` 新增别名按钮与 `AliasModal`（普通别名）；`/api/models/test`（测试全部按钮 + 后台探测复用）。

### 新增（聚合别名 Modal）

- 别名输入。
- **候选列表编辑区**：每行 = 账户下拉（`safeAccounts` 中 `is_active===1`）+ 模型名输入（`datalist` 建议自 `safeModels`）+ 上移/下移/删除。
- **顶部候选即默认**（重排改默认/顺序）。
- 每候选显示健康圆点（来自 `GET /api/aggregate-aliases` 返回的内存态 `last_status`）。
- 保存 → `POST /api/aggregate-aliases`。

新 store 动作：`fetchAggregateAliases`、`saveAggregateAlias`、`deleteAggregateAlias`（`ui/src/stores/models.ts`）。

## 改动文件清单

### Rust
- `crates/llmux-core/src/aggregate.rs`（新）：`AggregateCandidate`、`AggregateResolution`、`AggregateRouter`、`resolve_aggregate`（DB 查询从 pool）、`record_success/failure/probe_*`。
- `crates/llmux-core/src/dispatcher.rs`：`get_account_by_id`（若未抽取）。
- `crates/llmux-core/src/db.rs` + `crates/llmux-core/src/migrations/0007_add_aggregate_aliases.sql`（新）：迁移注册。
- `crates/llmux-server/src/app.rs`：`AppState.aggregate_router`；`resolve_aggregate_cached`（TTL 缓存，与 `resolve_model_cached` 同模式）；`core_router()` 注册 `/api/aggregate-aliases` 路由。
- `crates/llmux-server/src/routes/v1/openai.rs`：`dispatch_aggregate_openai` + 两入口短路。
- `crates/llmux-server/src/routes/v1/anthropic.rs`：`dispatch_aggregate_anthropic` + `messages` 短路。
- `crates/llmux-server/src/routes/models/aggregate.rs`（新）：CRUD。
- `crates/llmux-bin/src/main.rs`：`AppState` 初始化带 `aggregate_router`；`tokio::spawn` 后台探测循环。
- `crates/llmux-server/src/routes/v1/models.rs`：`/v1/models` 透出聚合别名（context_length 取默认候选）。

### TypeScript
- `ui/src/stores/models.ts`：`AggregateAlias` 接口 + 三个动作。
- `ui/src/routes/models.tsx`：移除自定义别名；新增聚合别名按钮 + Modal。
- `ui/src/components/Models/CustomAliasModal.tsx`：删除。
- `ui/src/i18n/*.json`：删除 `customAlias`/`verify`/`selectAccount`/`manualModel` key；新增聚合别名 key。

## 测试

- 合约测试（`crates/llmux-core/tests/core_contract.rs` 或新增 `aggregate_contract.rs`）：
  - `candidates` JSON 解析（空数组、缺字段、非法 account_id）。
  - `AggregateRouter` 状态机：`record_success` 粘住成功位、`record_failure` 降级 +1、`record_probe_success` 拉回 0、`record_probe_failure` 降级 +1、退避指数上限 600、降级到末尾不自愈成 0。
  - `resolve_aggregate`：普通别名命中 → None；未命中 → 聚合命中 → Some；都没命中 → None（走 prefix 兜底）。
- HTTP 集成（`crates/llmux-server`）：
  - `POST /api/aggregate-aliases` 合法/非法 body。
  - `GET /api/aggregate-aliases` 带 `current_index`。
  - `DELETE /api/aggregate-aliases/:id`。
- 派发（`llmux-core/tests/gateway_contract.rs` 或 server 集成，可选）：
  - 请求 `test`（3 候选）→ 默认候选失败 → 第 2 候选成功，响应模型 = 第 2 候选模型名。

## 验证计划

1. `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test` 通过。
2. 本地起服务（`DATA_DIR=/tmp/llmux-agg-test`），`curl POST /api/aggregate-aliases` 建 `test`，`curl GET /api/aggregate-aliases` 看到 `current_index`。
3. `curl -H "Authorization: Bearer <key>" -d '{"model":"test",...}' http://localhost:25976/v1/chat/completions`：默认候选断连时，响应模型为第 2 候选；`/v1/models` list 含 `test`。
4. 等 60s 探测周期：模拟默认候选恢复，观察 `current_index` 拉回 0。
5. UI：模型页按钮为 `[测试全部][刷新][新增别名][聚合别名]`；无自定义别名；聚合别名 Modal 可编辑候选、重排、删除。
6. `npm run build` + `cargo build --release` 通过；部署到路由器后线上 `https://openwrt.xiaokubao.space/llmux/models` 验证。

## 风险与回滚

- **迁移新增表**：`CREATE TABLE IF NOT EXISTS` 幂等，`init_db` 对已存在表容忍（现有模式）。回滚 = 删表 + 去掉 `aggregate_router`。
- **handler 短路**：普通别名 `resolve_aggregate` 返回 `None` 走原逻辑，行为不变。回滚 = 去掉短路分支。
- **后台探测消耗**：每 60s 每聚合别名一次 1-token 请求。可接受（用户已确认）；可通过 `interval_secs` 调大、或聚合别名为空时跳过。
- **状态机并发**：`AggregateRouter` 用 `std::sync::Mutex`，临界区 µs 级（与 `DispatchRouter` 同款），无 async 锁需求。
