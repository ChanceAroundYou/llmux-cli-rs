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

请求命中 `test` 时从当前活跃位 `V` 开始按固定顺序尝试（默认 `V=0`）；后台每 5 分钟以 `V` 为锚点做双阶段探测；**任何升/降级都需要连续 3 次确认才正式切换**（含请求侧与探测侧，见状态机），避免抖动；全部挂则 `V` 复位到 `0`、下一次请求走整链 `a→b→c→d` 兜底，避免"每个请求都先吃一次失败"。

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
  interval_secs INTEGER NOT NULL DEFAULT 300, -- 后台探测间隔，默认 5 分钟
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
- 运行时"当前活跃位 `V`"是**内存态**（`AggregateRouter`），不落库。进程重启即回到默认 `V=0`。
- 候选列表在写入时整体替换（`INSERT OR REPLACE`），天然原子。
- **写入（upsert）或删除聚合别名时，清除该别名的内存态 entry**（`aggregate_router.entries.remove(alias)`），下次请求重新初始化为默认 `V=0`。避免旧的活跃位指向新候选列表的错误位置。

### `AppState` 扩展

```rust
pub struct AggregateRouter {
    // alias -> Current state (active index V + probe state)
    entries: HashMap<String, AggregateEntry>,
}
pub struct AggregateEntry {
    active: usize,                 // 当前活跃位 V：下一次请求/探测的锚点
    probe_backoff_secs: u64,       // 探测整体失败时的指数退避（可选）
    last_probe: Instant,
    last_status: Vec<Option<bool>>,// 每候选最近一次探测结果，供 UI 逐行显示
    // --- 稳定化：3 次连续确认才升/降级 ---
    // pending_* 记录"若切换会切换到哪里"，与 confirm 计数联动；非 pending 时为 None
    pending_target: Option<usize>,  // 目标 V（探测阶段1/2给出的 new_v，或请求失败路径的 failed_index+1）
    confirm_count: u8,              // 对同一 pending_target 的连续确认次数（0..3）
    // pending_target 相同则 confirm_count++，不同则 pending_target 换为新目标且 confirm_count=1
    // 达到 3 次才执行 V = pending_target 并 pending_target=None/confirm_count=0
    // 任何一次请求成功直接 record_success 时，同步清 pending_target / confirm_count（稳定就绪）
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
    pub active: usize,        // 从 AggregateRouter 读出的活跃位 V
}
pub struct AggregateCandidate {
    pub account_id: i64,
    pub model: String,
}

pub async fn resolve_aggregate(pool, model_name, router: &AggregateRouter)
    -> anyhow::Result<Option<AggregateResolution>>
```

- 先查现有 `model_aliases`，命中普通别名 → 返回 `None`（走原逻辑）；未命中再查 `aggregate_aliases`，命中 → 返回 `Some(...)`；都没命中 → 走 `resolve_model_by_prefix` 兜底（原逻辑）。
- `active` 从 `AggregateRouter.entries[alias]` 读出；不含该 entry 时 `= 0`（默认）。

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

核心差异：每跳一个候选，`body["model"]` 用**该候选的真实模型名**覆盖，然后构造请求。请求侧**始终尽力在当次请求内完成可用候选的兜底**，`V` 的迁移则经 3 次确认稳定化。伪代码（以 `S: [a,b,c,d]`、当前活跃位 `V` 为例）：

```
// 请求侧：从 V 开始向下游滑，直到成功或全链耗尽；每次请求仍尽力兜底成功
let mut first_success: Option<usize> = None;
for i in agg.active..agg.candidates.len() {
    let cand = &agg.candidates[i];
    let account = get_account_by_id(pool, cand.account_id, master_key)?;
    if account.is_none() or inactive { router.note_candidate_failure(alias, i); continue; }
    let mut patched = body.clone();
    patched["model"] = cand.model;                       // 关键：每步换模型名
    ...build ProviderRequest (base_url per provider_id)...
    let resp = execute_provider_request(req).await;
    if ok {
        first_success = Some(i);
        ...usage logging / streaming passthrough (复用现有原语)...
        break;
    }
    router.note_candidate_failure(alias, i);             // 记录该候选本次失败，供 V 决策参考
    continue;
}
if let Some(hit) = first_success {
    // 命中位 hit（可能 == V，也可能 > V 是降级命中）
    // 稳定化：hit != V 时才涉及 V 迁移，需 3 次连续同目标才生效
    router.record_request_outcome(alias, old_v = agg.active, hit);
    return resp;
}
// 全链全挂：本轮返回 502；V 的复位同样需 3 次连续全挂才生效
router.record_request_all_failed(alias);
return 502 all exhausted
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

### 降级/恢复状态机（`AggregateRouter`，以活跃位 `V` 为锚，3 次确认稳定化）

设聚合别名 `S: [a(0), b(1), c(2), d(3)]`，`V` 为当前活跃位（初始 `V=0`）。任何使 `V` 迁移的结论都需**连续 3 次同目标复现**才生效；单次抖动只记一次，不切。

**请求侧**（当次请求仍尽力兜底成功，但 `V` 的迁移经确认）：

- 记法：`pending_target: Option<usize>` 与 `confirm_count: u8`（见 `AggregateEntry`）。
- `note_candidate_failure(alias, failed_index)`：仅刷新 `last_status[failed_index]=Some(false)`，不直接改 `V`。
- `record_request_outcome(alias, old_v, hit_index)`：当次请求在 `old_v..len` 内首个成功位为 `hit`（`hit == old_v` 表示默认仍活，无需迁移；`hit > old_v` 表示需降到 `hit`；无 hit 则走 `record_request_all_failed`）。
  - 若 `hit == old_v`：`pending_target`/`confirm_count` 清零（`None`/`0`），`last_status[hit]=Some(true)`，退避重置。
  - 若 `hit != old_v`：把 `hit` 作为待迁移目标走确认器
    - `pending_target == Some(hit)` → `confirm_count += 1`
    - `pending_target != Some(hit)` → `pending_target = Some(hit)`, `confirm_count = 1`
    - `confirm_count >= 3` → `V = hit`, `pending_target=None`, `confirm_count=0`, `last_status[hit]=Some(true)`，退避重置。
    - 未满 3 次：`V` 保持 `old_v` 不变（当次请求已在 `hit` 上成功返回，但锚点下次才切，避免单次毛刺切走默认）。
- `record_request_all_failed(alias)`：全链 502。以 `0`（复位到默认）作为待迁移目标同样走 3 次确认；确认期间请求侧仍每次 `for i in V..len` 重试全链兜底。
- 任何一次 `hit != None`（请求成功）若 `V` 未迁移，也刷新 `last_status[hit]`；`record_success` 的旧称保留为上述两个方法的组合。

**探测侧**（后台周期 `interval_secs = 300`，以 `V` 为锚——先往上升级，再往下寻活，结论同需 3 次确认）：

```
给定当前 V，候选 S = [0..len-1]

阶段1（升级扫描）：对 indices [0..=V] 的候选**并发**发轻量 ping（max_tokens=1），
              若其中有若干通过，取**最小 index（优先级最高）**作为候选新 V'。
              若阶段1有命中：候选 V' = min{通过的 index}。
              若阶段1全挂：进入阶段2。

阶段2（下探扫描）：仅当阶段1全挂时执行。对 indices [V+1 .. len-1] 顺序发 ping，
              首个通过的即候选 V' = that_index。

全链挂：若两阶段皆全挂 → 候选 V' = 0（复位到默认）。
```

探测得到候选 `V'` 后，不立即 `V = V'`，而是以 `V'` 为 `pending_target` 走同一确认器：

- `pending_target == Some(V')` → `confirm_count += 1`
- `pending_target != Some(V')` → `pending_target = Some(V')`, `confirm_count = 1`
- `confirm_count >= 3` → `V = V'`, `pending_target=None`, `confirm_count=0`。若 `V'` 来自全链挂（`V'=0` 且当时 `V != 0` 久挂），视作降级类全挂轮，退避 `min(secs*2, 600)`；否则命中轮退避重置回 60。
- 未满 3 次：本轮探测"得到结论但不切"，**立即按同一 V 重试一次**（短间隔，如 5s 内）直到连续复现 3 次或结论翻转；若结论翻转则按新 `V''` 重置 `confirm_count=1`。实现上"连续 3 次"可在**连续 3 个探测周期**内累计，也可在同周期内短间隔重试 3 次——二选一，默认选**跨周期累计**（更稳、更省 token），短间隔重试作为可选优化在实现时决定。

- `last_status[j]` 每轮被对应探测结果刷新，供 `GET /api/aggregate-aliases` 逐行显示。
- 上述"阶段1并发、阶段2顺序"是建议实现；最小实现也可两阶段都顺序——行为相同，只是探测耗时不同。

**退避**（整别名级，而非单候选级）：`probe_backoff_secs` 初始 300（与探测间隔对齐）；仅当**本轮两阶段全挂且连续 3 次确认后才真正计为一次失败轮**时 `min(secs*2, 600)`；任意一阶段命中且经 3 次确认切 `V` 后重置回 300。请求成功（`record_request_outcome` 命中）同样重置退避到 300。未满 3 次的 pending 期间不改退避。

> 简言之：当次请求**仍尽力在链上滑到底并把成功的那个返回给用户**；但 `V` 的锚点迁移（无论是请求侧的降级还是探测侧的升/降级）都要**连续 3 次同目标**才正式切，避免单次超时抖动把默认切走。

## 后台探测（每 5 分钟，以 `V` 为锚的双阶段扫描，结论 3 次确认）

`tokio::spawn` 后台循环，挂在 `main.rs`（与测试队列同层）：

```
loop {
   // 对每个聚合别名 S:
   for (alias, entry) in router.snapshot() {
      let V = entry.active;
      // 探测请求统一是"轻量真实请求"：max_tokens=1 + timeout 10s，
      // 按候选所属账户的协议分支构造：
      //  - account 走 Anthropic：POST /v1/messages  { model: cand.model, max_tokens:1, messages:[{role:"user",content:"ping"}] }
      //  - 否则：            POST /chat/completions { model: cand.model, max_tokens:1, messages:[{role:"user",content:"ping"}] }
      // 如何判定"活着"：status.is_success() 且非 retryable（沿用 is_retryable_status 语义：401/403/429 算"死了"重试到下个；关键是 2xx）

      // 阶段1：对 indices [0..=V] 并发探测，命中取最小 index → 候选 V1
      let stage1 = probes_for(S, 0..=V).concurrent(..);
      let candidate_v: Option<usize> = if let Some(v1) = stage1.min_alive_index() {
         Some(v1)
      } else {
         // 阶段2：阶段1全挂才执行。对 [V+1 .. len-1] 顺序探测，命中首个即候选 V1
         let stage2 = probes_for(S, V+1..len).sequential(..);
         if let Some(v2) = stage2.first_alive_index() { Some(v2) } else { Some(0) } // 全链挂候选 0
      };
      let v_prime = candidate_v.unwrap(); // 本轮探测给出的候选新 V

      // 3 次确认才真正切 V（与请求侧共用 pending_target/confirm_count）
      let switched = router.record_probe_candidate(alias, v_prime);
      // switched == true 表示本轮刚好第 3 次确认并已 V = v_prime
      // switched == false 表示还在累计 pending（1/3、2/3），V 保持不变
      // 若 v_prime 的结论与上一轮 pending_target 不同，router 内部会重置 confirm_count=1
   }
   sleep(interval_secs)   // 默认 300s（5 分钟）；若刚写入"全链挂"且已 3 次确认，实际休眠 min(probe_backoff_secs, interval_secs)
}
```

- 每轮每别名最多探测 `len(S)` 次（`<= 阶段1(V+1) + 阶段2(len-V-1)`，并发不改变次数）。
- 阶段1"并发"是建议实现（更快收敛到最优 V）；最小实现可两阶段都顺序扫描，行为一致。
- `GET /api/aggregate-aliases` 返回每别名的 `active`（即 `V`）与 `last_status`（每候选 Recent 结果）以及 `pending_target`/`confirm_count`（可选，供 UI 显示"待切到 X (1/3)"的稳定化进度），`models.tsx` 逐行圆点使用它。
- 探测的"连续 3 次"默认**跨周期累计**（更省 token、更稳）；若需更快收敛，可在同周期内对同一 `v_prime` 短间隔重试 2 次补齐 3 次——二选一，实现时定。

## HTTP API

新路由（`crates/llmux-server/src/routes/models/aggregate.rs`），注册进 `core_router()`：

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/aggregate-aliases` | 列表，每项带 `active`（当前 `V`）+ 候选数组 + 每候选 `last_status` |
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
- 每候选显示健康圆点（来自 `GET /api/aggregate-aliases` 返回的 `last_status`），当前活跃位 `active` 高亮；若该别名有 `pending_target`（待切目标），额外显示"待切到 X (m/3)"的稳定化进度。
- 保存 → `POST /api/aggregate-aliases`。

新 store 动作：`fetchAggregateAliases`、`saveAggregateAlias`、`deleteAggregateAlias`（`ui/src/stores/models.ts`）。

## 改动文件清单

### Rust
- `crates/llmux-core/src/aggregate.rs`（新）：`AggregateCandidate`、`AggregateResolution`、`AggregateRouter`（含 `pending_target`/`confirm_count` 3 次确认）、`resolve_aggregate`（DB 查询从 pool）、`record_request_outcome`/`record_request_all_failed`/`record_probe_candidate`。
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
- `ui/src/routes/models.tsx`：移除自定义别名；新增聚合别名按钮 + Modal（候选列表编辑区，显示每行圆点来自 `last_status`，当前活跃位 `active` 高亮）。
- `ui/src/components/Models/CustomAliasModal.tsx`：删除。
- `ui/src/i18n/*.json`：删除 `customAlias`/`verify`/`selectAccount`/`manualModel` key；新增聚合别名 key。

## 测试

- 合约测试（`crates/llmux-core/tests/core_contract.rs` 或新增 `aggregate_contract.rs`）：
  - `candidates` JSON 解析（空数组、缺字段、非法 account_id）。
  - `AggregateRouter` 3 次确认稳定化：同目标连续 3 次才切 `V`，不同目标重置 `confirm_count`，成功立即清 pending；探测侧阶段1取最小存活 V，阶段2取首个存活 V，两阶段全挂且连续 3 次确认后 `V=0`（未满 3 次不切）；退避仅全挂且确认后翻倍（上限 600），任意命中经确认后重置回 300。
  - `resolve_aggregate`：普通别名命中 → None；未命中 → 聚合命中 → Some；都没命中 → None（走 prefix 兜底）。
- HTTP 集成（`crates/llmux-server`）：
  - `POST /api/aggregate-aliases` 合法/非法 body。
  - `GET /api/aggregate-aliases` 带 `active` 与 `pending_target`/`confirm_count`。
  - `DELETE /api/aggregate-aliases/:id`。
- 派发（`llmux-core/tests/gateway_contract.rs` 或 server 集成，可选）：
  - 请求 `test`（3 候选）→ 默认候选失败 → 第 2 候选成功，响应模型 = 第 2 候选模型名。

## 验证计划

1. `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test` 通过。
2. 本地起服务（`DATA_DIR=/tmp/llmux-agg-test`），`curl POST /api/aggregate-aliases` 建 `test`，`curl GET /api/aggregate-aliases` 看到 `active` 与 `pending_target`。
3. `curl -H "Authorization: Bearer <key>" -d '{"model":"test",...}' http://localhost:25976/v1/chat/completions`：默认候选断连时，单次请求仍兜底到第 2 候选成功，但 `V` 未立即切；连续 3 次同位命中后 `V` 才切。
4. 等 5 分钟探测周期（或临时把 `interval_secs` 调小验证）：模拟默认候选恢复，观察经 3 次探测确认后 `V` 升回 0。
5. UI：模型页按钮为 `[测试全部][刷新][新增别名][聚合别名]`；无自定义别名；聚合别名 Modal 可编辑候选、重排、删除。
6. `npm run build` + `cargo build --release` 通过；部署到路由器后线上 `https://openwrt.xiaokubao.space/llmux/models` 验证。

## 风险与回滚

- **迁移新增表**：`CREATE TABLE IF NOT EXISTS` 幂等，`init_db` 对已存在表容忍（现有模式）。回滚 = 删表 + 去掉 `aggregate_router`。
- **handler 短路**：普通别名 `resolve_aggregate` 返回 `None` 走原逻辑，行为不变。回滚 = 去掉短路分支。
- **后台探测消耗**：每 300s（5 分钟）每聚合别名最多一次双阶段扫描（<= len 次 1-token 请求），只有全链挂且经 3 次确认后才退避翻倍。可接受；可通过 `interval_secs` 调大、或聚合别名为空时跳过。
- **状态机并发**：`AggregateRouter` 用 `std::sync::Mutex`，临界区 µs 级（与 `DispatchRouter` 同款），无 async 锁需求。
