# 上下游拆分：Account 多端点 + Alias 下游路由模式 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 llmux 拆为上游 Account 三端点（`chat_endpoint / responses_endpoint / messages_endpoint + default_protocol`）与下游 Alias 下游路由模式（`default | chat | responses | messages`）两层，完成 DB 迁移、协议决策、网关三入口收敛与前端可配置，支持回滚且不替换现有容器/数据卷。

**Architecture:** 新增 `crates/llmux-core/src/protocol.rs` 承载纯函数决策表 `target_protocol / endpoint_for / supports`；Account 模型与 adapters 按协议选端点拼 URL；`proxy::{responses, anthropic_openai, openai_anthropic}` 复用，仅由 `match (ingress, target)` 决定是否转换与走哪一对；Server 三入口统一 `resolve → target → passthrough|convert`，聚合候选逐一算 target；前端三勾选与下游模式下拉联动校验。

**Tech Stack:** Rust (axum, sqlx/sqlite, serde_json), SQLite migrations, React + TypeScript (Vite, shadcn/ui), existing proxy converters (`ResponsesTo*Converter`, `OpenAISseConverter`).

## Global Constraints

- 共享单 `api_key`：三端点复用同一 `accounts.api_key`，不新增 key 列。
- 至少一端点：`POST/PUT /api/accounts` 至少一个 `*_endpoint` 非空，否则 `400 account_no_endpoint`。
- `default_protocol` 必须在已启用集合内，否则 `400 invalid_default_protocol`。
- 不引入第 4 协议；不改计费/限额；不动 `DispatchRouter` 粘滞策略。
- 不替换现有容器/数据卷：沿用 `deploy/deploy.sh`（`cargo build --release → scp llmux → docker compose build && up -d`），`DATA_DIR=/data/llmux` 与 `master.key` 铁律不变。
- `accounts.base_url / anthropic_base_url` 保留只读作回滚锚点，新代码不再写入它们。
- DB 列名 `model_aliases.upstream_api / aggregate_aliases.upstream_api` 保持不变，Rust 侧重命名为 `downstream_mode` 语义。
- `UpstreamApi::Auto` 废弃：`from_str("auto")` 读作 `default`，写路径永不产出 `auto`，migration 将存量 `auto` 置 `default`。

---

## File Structure

**Created:**
- `crates/llmux-core/src/protocol.rs` — `Protocol`, `DownstreamMode`, `target_protocol`, `endpoint_for`, `supports` 纯函数 + 单测。
- `crates/llmux-core/src/migrations/0009_account_endpoints.sql` — 三列 + default 列 + 回填 + `auto→default` 迁移。
- `crates/llmux-core/tests/protocol_contract.rs` — 决策表合约测试（可选，或并入 `core_contract.rs`）。

**Modified:**
- `crates/llmux-core/src/models.rs` — `Account` / `AccountPublic` 新增四字段。
- `crates/llmux-core/src/adapters/mod.rs` — `Account` 同步字段、`get_active_accounts` / `get_accounts_by_ids` 显式 SELECT 新列、`build_passthrough`、按 `default_protocol` 探测。
- `crates/llmux-core/src/db.rs` — 注册 `MIGRATION_0009`。
- `crates/llmux-core/src/lib.rs` — `pub mod protocol;` 并保留 `pub mod upstream_api` 作兼容 shim。
- `crates/llmux-core/src/upstream_api.rs` — `Auto` 标记 deprecated，`from_str("auto")` → `default` 兼容。
- `crates/llmux-server/src/routes/accounts.rs` — 校验 + 批量回落事务 + 返回 `affectedAliases`。
- `crates/llmux-server/src/routes/models/aliases.rs` — 强制校验（`409 alias_protocol_unsupported`）。
- `crates/llmux-server/src/routes/models/aggregate.rs` — 同上，逐候选校验。
- `crates/llmux-server/src/routes/v1/anthropic.rs` — `messages` 入口 `match (ingress,target)` 收敛。
- `crates/llmux-server/src/routes/v1/openai.rs` — `chat/completions` 与 `responses` 入口同上。
- `crates/llmux-server/src/routes/v1/helpers.rs` — 若有抽取 `dispatch_with_conversion` 则新增。
- `ui/src/routes/accounts.tsx` — 三勾选 EndpointRow + default_protocol 联动。
- `ui/src/routes/models.tsx` — `downstreamMode` 下拉 + 账户过滤。
- `ui/src/stores/accounts.ts` / `models.ts` / `ui/src/lib/api.ts` — 新字段透传。

---

### Task 1: Protocol 决策表（纯函数）

**Files:**
- Create: `crates/llmux-core/src/protocol.rs`
- Create: `crates/llmux-core/tests/protocol_contract.rs` (or extend `core_contract.rs`)
- Modify: `crates/llmux-core/src/lib.rs:1-15`
- Modify: `crates/llmux-core/src/upstream_api.rs:1-49` (deprecate Auto)

**Interfaces:**
- Consumes: `adapters::Account { chat_endpoint, responses_endpoint, messages_endpoint, default_protocol }`
- Produces:
  - `pub enum Protocol { Chat, Responses, Messages }` with `as_str/from_str`
  - `pub enum DownstreamMode { Default, Chat, Responses, Messages }` with `from_str` (maps `auto→Default`, `default→Default`)
  - `pub fn supports(account: &Account, p: Protocol) -> bool`
  - `pub fn endpoint_for(account: &Account, p: Protocol) -> Option<&str>`
  - `pub fn target_protocol(ingress: Protocol, mode: DownstreamMode, account: &Account) -> Protocol`
  - `pub fn default_protocol_for(account: &Account) -> Protocol` (parse `default_protocol` string, fallback Chat)

- [ ] **Step 1: Write the failing test** — `crates/llmux-core/tests/protocol_contract.rs`

```rust
use llmux_core::protocol::{Protocol, DownstreamMode, target_protocol, supports};
use llmux_core::adapters::Account;

fn acc(chat: Option<&str>, resp: Option<&str>, msg: Option<&str>, def: &str) -> Account {
    Account { id: 1, alias: "x".into(), provider_id: "custom".into(), api_key: "k".into(),
        base_url: None, anthropic_base_url: None,
        chat_endpoint: chat.map(|s| s.to_string()), responses_endpoint: resp.map(|s| s.to_string()),
        messages_endpoint: msg.map(|s| s.to_string()), default_protocol: def.into(),
        is_active: 1, weight: 1, openai_compatible: 0 }
}

#[test]
fn default_mode_passthrough_when_supported() {
    let a = acc(Some("https://a/v1"), Some("https://a/v1"), Some("https://a/v1"), "chat");
    assert_eq!(target_protocol(Protocol::Chat, DownstreamMode::Default, &a), Protocol::Chat);
    assert_eq!(target_protocol(Protocol::Messages, DownstreamMode::Default, &a), Protocol::Messages);
}

#[test]
fn default_mode_falls_back_to_default_protocol() {
    let a = acc(Some("https://a/v1"), None, None, "chat");
    assert_eq!(target_protocol(Protocol::Responses, DownstreamMode::Default, &a), Protocol::Chat);
    assert_eq!(target_protocol(Protocol::Messages, DownstreamMode::Default, &a), Protocol::Chat);
}

#[test]
fn forced_mode_always_targets_forced() {
    let a = acc(Some("https://a/v1"), Some("https://a/v1"), None, "chat");
    assert_eq!(target_protocol(Protocol::Chat, DownstreamMode::Responses, &a), Protocol::Responses);
    assert_eq!(target_protocol(Protocol::Messages, DownstreamMode::Responses, &a), Protocol::Responses);
}

#[test]
fn auto_maps_to_default() {
    assert_eq!(DownstreamMode::from_str("auto"), DownstreamMode::Default);
    assert_eq!(DownstreamMode::from_str("default"), DownstreamMode::Default);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `~/.cargo/bin/cargo test -p llmux-core protocol -- --nocapture`
Expected: FAIL with `could not find protocol` / `unresolved import`

- [ ] **Step 3: Write minimal implementation** — `crates/llmux-core/src/protocol.rs`

```rust
use crate::adapters::Account;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol { Chat, Responses, Messages }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownstreamMode { Default, Chat, Responses, Messages }

impl DownstreamMode {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "chat" => Self::Chat, "responses" => Self::Responses, "messages" => Self::Messages,
            "auto" | "default" | "" => Self::Default, _ => Self::Default,
        }
    }
}

pub fn supports(a: &Account, p: Protocol) -> bool { endpoint_for(a, p).is_some() }
pub fn endpoint_for(a: &Account, p: Protocol) -> Option<&str> {
    match p {
        Protocol::Chat => a.chat_endpoint.as_deref(),
        Protocol::Responses => a.responses_endpoint.as_deref(),
        Protocol::Messages => a.messages_endpoint.as_deref(),
    }.filter(|s| !s.is_empty())
}
pub fn target_protocol(ingress: Protocol, mode: DownstreamMode, account: &Account) -> Protocol {
    match mode {
        DownstreamMode::Default => if supports(account, ingress) { ingress } else { default_protocol_for(account) },
        DownstreamMode::Chat => Protocol::Chat, DownstreamMode::Responses => Protocol::Responses, DownstreamMode::Messages => Protocol::Messages,
    }
}
pub fn default_protocol_for(a: &Account) -> Protocol {
    match a.default_protocol.as_deref().unwrap_or("chat") {
        "responses" => Protocol::Responses, "messages" => Protocol::Messages, _ => Protocol::Chat,
    }
}
```

Wire `lib.rs`: `pub mod protocol;` keep `pub mod upstream_api;` as shim re-exporting `DownstreamMode`.

In `upstream_api.rs`: add `#[deprecated] Auto` and change `from_str("auto") => Self::Chat` to `Self::Chat` with comment mapping to Default (or delegate to `protocol::DownstreamMode::from_str`).

- [ ] **Step 4: Run test to verify it passes**

Run: `~/.cargo/bin/cargo test -p llmux-core protocol -- --nocapture`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/llmux-core/src/protocol.rs crates/llmux-core/src/lib.rs crates/llmux-core/src/upstream_api.rs crates/llmux-core/tests/protocol_contract.rs
git commit -m "feat(core): protocol decision table for upstream/downstream split"
```

---

### Task 2: DB Migration 0009 + Core Models/Adapeters 显式列

**Files:**
- Create: `crates/llmux-core/src/migrations/0009_account_endpoints.sql`
- Modify: `crates/llmux-core/src/db.rs:1-15, 32-70`
- Modify: `crates/llmux-core/src/models.rs:1-72`
- Modify: `crates/llmux-core/src/adapters/mod.rs:51-62, 360-460`

**Interfaces:**
- Consumes: Task 1 `protocol` helpers (for validation)
- Produces: `Account` with `chat_endpoint/responses_endpoint/messages_endpoint/default_protocol`, `AccountPublic` same, SELECTs include new columns, migration backfill.

- [ ] **Step 1: Write the failing test** — extend `crates/llmux-core/tests/core_contract.rs`

```rust
#[tokio::test]
async fn migration_0009_adds_endpoints_and_backfills() {
    let pool = memory_db().await; // init_db already runs migrations
    // after init_db, columns must exist and at least one account inserted via old base_url is backfilled
    let cols: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('accounts') WHERE name IN ('chat_endpoint','responses_endpoint','messages_endpoint','default_protocol') ORDER BY name")
        .fetch_all(&pool).await.unwrap();
    assert!(cols.contains(&"chat_endpoint".to_string()));
    assert!(cols.contains(&"default_protocol".to_string()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `~/.cargo/bin/cargo test -p llmux-core migration_0009 -- --nocapture`
Expected: FAIL — column not found

- [ ] **Step 3: Write minimal implementation**

`migrations/0009_account_endpoints.sql`:
```sql
ALTER TABLE accounts ADD COLUMN chat_endpoint TEXT;
ALTER TABLE accounts ADD COLUMN responses_endpoint TEXT;
ALTER TABLE accounts ADD COLUMN messages_endpoint TEXT;
ALTER TABLE accounts ADD COLUMN default_protocol TEXT DEFAULT 'chat';
UPDATE accounts SET
  chat_endpoint = CASE WHEN provider_id IN ('openai','gemini','custom') THEN base_url END,
  messages_endpoint = CASE WHEN provider_id IN ('anthropic','custom-anthropic') THEN COALESCE(anthropic_base_url, base_url)
                           WHEN provider_id='custom' AND anthropic_base_url IS NOT NULL THEN anthropic_base_url END,
  responses_endpoint = NULL,
  default_protocol = CASE WHEN chat_endpoint IS NOT NULL OR base_url IS NOT NULL THEN 'chat'
                          WHEN messages_endpoint IS NOT NULL THEN 'messages' ELSE 'chat' END
WHERE chat_endpoint IS NULL AND messages_endpoint IS NULL AND responses_endpoint IS NULL;
-- fixup where CASE left chat_endpoint NULL but base_url existed (custom without provider match):
UPDATE accounts SET chat_endpoint = base_url WHERE chat_endpoint IS NULL AND base_url IS NOT NULL AND provider_id='custom' AND messages_endpoint IS NULL;
UPDATE accounts SET default_protocol = CASE WHEN chat_endpoint IS NOT NULL THEN 'chat' WHEN messages_endpoint IS NOT NULL THEN 'messages' ELSE 'chat' END WHERE default_protocol IS NULL OR default_protocol='';
UPDATE model_aliases SET upstream_api='default' WHERE upstream_api='auto';
UPDATE aggregate_aliases SET upstream_api='default' WHERE upstream_api='auto';
```

`db.rs`: add `pub const MIGRATION_0009: &str = include_str!("migrations/0009_account_endpoints.sql");` and include `("0009", MIGRATION_0009)` in migrations array.

`models.rs`: add to `Account` and `AccountPublic`:
```rust
pub chat_endpoint: Option<String>,
pub responses_endpoint: Option<String>,
pub messages_endpoint: Option<String>,
pub default_protocol: Option<String>,
```

`adapters/mod.rs`: extend `Account` struct same four fields, update `From` impls, update both `SELECT ... base_url, anthropic_base_url` queries to `..., chat_endpoint, responses_endpoint, messages_endpoint, default_protocol`, and map them in row→Account construction.

- [ ] **Step 4: Run test to verify it passes**

Run: `~/.cargo/bin/cargo test -p llmux-core core_contract -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/llmux-core/src/migrations/0009_account_endpoints.sql crates/llmux-core/src/db.rs crates/llmux-core/src/models.rs crates/llmux-core/src/adapters/mod.rs
git commit -m "feat(db): add account multi-endpoint columns with backfill and auto→default migration"
```

---

### Task 3: Adapters `build_passthrough` 与探测收敛

**Files:**
- Modify: `crates/llmux-core/src/adapters/mod.rs:159-291`
- Test: `crates/llmux-core/tests/gateway_contract.rs`

**Interfaces:**
- Consumes: `protocol::endpoint_for / default_protocol_for`, `Account` new fields
- Produces:
  - `pub fn build_passthrough(account: &Account, protocol: Protocol, body: &Value) -> ProviderRequest` — selects `chat_endpoint/responses_endpoint/messages_endpoint`, appends `/chat/completions|/responses|/v1/messages`, sets `Authorization: Bearer {api_key}`.
  - `test_provider_connection` now probes `endpoint_for(default_protocol_for)` instead of `provider_id` branching.

- [ ] **Step 1: Write the failing test** — `gateway_contract.rs`

```rust
#[test]
fn build_passthrough_selects_endpoint_by_protocol() {
    let acc = Account { id: 1, alias: "x".into(), provider_id: "custom".into(), api_key: "sk".into(),
        base_url: Some("https://old/v1".into()), anthropic_base_url: None,
        chat_endpoint: Some("https://a.example/v1".into()), responses_endpoint: Some("https://a.example/v1".into()),
        messages_endpoint: Some("https://a.example/v1".into()), default_protocol: Some("chat".into()),
        is_active: 1, weight: 1, openai_compatible: 0 };
    let req = llmux_core::adapters::build_passthrough(&acc, Protocol::Messages, &json!({"model":"m"}));
    assert!(req.url.ends_with("/v1/messages"));
    let req2 = llmux_core::adapters::build_passthrough(&acc, Protocol::Chat, &json!({"model":"m"}));
    assert!(req2.url.ends_with("/chat/completions"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `~/.cargo/bin/cargo test -p llmux-core gateway_contract -- --nocapture`
Expected: FAIL — function not found

- [ ] **Step 3: Write minimal implementation** — `adapters/mod.rs`

```rust
pub fn build_passthrough(account: &Account, protocol: crate::protocol::Protocol, body: &Value) -> ProviderRequest {
    let proto = protocol;
    let base = crate::protocol::endpoint_for(account, proto).unwrap_or("https://api.openai.com/v1");
    let base = normalize_base_url(base);
    let suffix = match proto { Protocol::Chat => "chat/completions", Protocol::Responses => "responses", Protocol::Messages => "v1/messages" };
    let url = if base.ends_with("/v1") && suffix.starts_with("v1/") { format!("{}/{}", base, &suffix[3..]) } else { format!("{base}/{suffix}") };
    let mut headers = json_headers();
    headers.insert("authorization".into(), format!("Bearer {}", account.api_key));
    ProviderRequest { method: "POST".into(), url, headers, body: body.clone() }
}
```

Keep `build_openai_passthrough` / `build_anthropic_passthrough` as thin wrappers calling `build_passthrough` for backwards compat.

Update `test_provider_connection` to:
```rust
let proto = crate::protocol::default_protocol_for(account);
let base = crate::protocol::endpoint_for(account, proto).unwrap_or("https://api.openai.com/v1");
```

- [ ] **Step 4: Run test to verify it passes**

Run: `~/.cargo/bin/cargo test -p llmux-core gateway_contract -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/llmux-core/src/adapters/mod.rs crates/llmux-core/tests/gateway_contract.rs
git commit -m "feat(adapters): select upstream endpoint by protocol and default_protocol"
```

---

### Task 4: Server — Account 校验与批量回落事务

**Files:**
- Modify: `crates/llmux-server/src/routes/accounts.rs`
- Test: `crates/llmux-server/tests/server_contract.rs` (add cases)

**Interfaces:**
- Consumes: `protocol::supports`, `Account` new fields
- Produces: `POST/PUT /api/accounts` 400 on invalid, `PUT /api/accounts/:id` with endpoint removal triggers atomic fallback of affected aliases.

- [ ] **Step 1: Write the failing test** — `server_contract.rs`

Add helper that POSTs account with zero endpoints and asserts 400; PUT that clears last endpoint asserts 400; PUT that clears `responses_endpoint` asserts 200 with `affectedAliases.ordinary` containing an alias previously in `responses` mode.

- [ ] **Step 2: Run test to verify it fails**

Run: `~/.cargo/bin/cargo test -p llmux-server server_contract -- --nocapture`
Expected: FAIL — server still accepts zero-endpoint

- [ ] **Step 3: Write minimal implementation** — `routes/accounts.rs`

In `create_account` and `update_account` handlers, after parsing body `{ chat_endpoint, responses_endpoint, messages_endpoint, default_protocol }`:
```rust
let enabled: Vec<Protocol> = [("chat",&chat_endpoint),("responses",&responses_endpoint),("messages",&messages_endpoint)]
    .into_iter().filter_map(|(k,v)| v.as_deref().filter(|s| !s.trim().is_empty()).map(|_| match k {"chat"=>Protocol::Chat,"responses"=>Protocol::Responses,_=>Protocol::Messages})).collect();
if enabled.is_empty() { return Err(api_error(400, "account_no_endpoint", "At least one endpoint is required")); }
let def = default_protocol.as_deref().unwrap_or("chat");
let def_proto = match def {"responses"=>Protocol::Responses,"messages"=>Protocol::Messages,_=>Protocol::Chat};
if !enabled.contains(&def_proto) { return Err(api_error(400, "invalid_default_protocol", "default_protocol must be one of the enabled protocols")); }
// normalize: trim trailing slash, validate URL::parse
```

On `PUT` where a previously non-null column becomes null, collect `removed: Vec<Protocol>`. If non-empty, run in `BEGIN IMMEDIATE`:
```rust
for proto in removed {
  let s = proto.as_str(); // "chat" etc.
  sqlx::query("UPDATE model_aliases SET upstream_api='default' WHERE upstream_api=?1 AND (account_ids LIKE ?2 OR (account_ids IS NULL AND provider_id=(SELECT provider_id FROM accounts WHERE id=?3)))")
    .bind(s).bind(format!("%{}%", id)).bind(id).execute(&mut *tx).await?;
  sqlx::query("UPDATE aggregate_aliases SET upstream_api='default' WHERE upstream_api=?1 AND EXISTS (SELECT 1 FROM json_each(candidates) WHERE json_extract(value,'$.account_id')=?2)")
    .bind(s).bind(id).execute(&mut *tx).await?;
  // also fix default_protocol if it was the removed one
  // pick fallback chat>messages>responses among remaining enabled
}
```

Return `Json(json!({"affectedAliases": {"ordinary": [...], "aggregate": [...]}, "newDefaultProtocol": ...}))`. Keep old `base_url/anthropic_base_url` columns untouched.

- [ ] **Step 4: Run test to verify it passes**

Run: `~/.cargo/bin/cargo test -p llmux-server server_contract -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/llmux-server/src/routes/accounts.rs crates/llmux-server/tests/server_contract.rs
git commit -m "feat(server): validate account endpoints and bulk-fallback aliases on endpoint removal"
```

---

### Task 5: Server — Alias/Aggregate 强制校验（409）

**Files:**
- Modify: `crates/llmux-server/src/routes/models/aliases.rs`
- Modify: `crates/llmux-server/src/routes/models/aggregate.rs`
- Test: `crates/llmux-server/tests/server_contract.rs`

**Interfaces:**
- Consumes: `protocol::supports`, `dispatcher::get_accounts_by_ids`, `accounts` table
- Produces: `409 alias_protocol_unsupported` when `downstream_mode != default` and any bound account lacks that protocol.

- [ ] **Step 1: Write the failing test**

Create account with only `chat_endpoint`, then POST alias with `upstream_api="responses"` bound to that account → expect 409 with `unsupportedAccounts`.

- [ ] **Step 2: Run test to verify it fails**

Run: `~/.cargo/bin/cargo test -p llmux-server server_contract -- --nocapture`
Expected: FAIL — server returns 200

- [ ] **Step 3: Write minimal implementation**

In both alias create/update and aggregate create/update, before INSERT:
```rust
let mode = DownstreamMode::from_str(body.upstream_api.as_deref().unwrap_or("default"));
if mode != DownstreamMode::Default {
  let proto = match mode { DownstreamMode::Chat=>Protocol::Chat, DownstreamMode::Responses=>Protocol::Responses, DownstreamMode::Messages=>Protocol::Messages, _=>unreachable!() };
  let accounts = if !account_ids.is_empty() { get_accounts_by_ids(&pool, &account_ids, &master_key).await? } else { get_active_accounts(&pool, provider_id.as_deref(), &master_key).await? };
  let unsupported: Vec<i64> = accounts.iter().filter(|a| !supports(a, proto)).map(|a| a.id).collect();
  if !unsupported.is_empty() {
    return Err(api_error_with_body(409, json!({"code":"alias_protocol_unsupported","field":"downstream_mode","unsupportedAccounts": unsupported}))); 
  }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `~/.cargo/bin/cargo test -p llmux-server server_contract -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/llmux-server/src/routes/models/aliases.rs crates/llmux-server/src/routes/models/aggregate.rs crates/llmux-server/tests/server_contract.rs
git commit -m "feat(server): reject alias with forced protocol unsupported by bound accounts"
```

---

### Task 6: Server — 三入口统一 `match (ingress, target)` 与运行时回退

**Files:**
- Modify: `crates/llmux-server/src/routes/v1/openai.rs` (`chat_completions`, `responses` handlers, `dispatch_*` helpers)
- Modify: `crates/llmux-server/src/routes/v1/anthropic.rs` (`messages` handler)
- Modify: `crates/llmux-server/src/routes/v1/helpers.rs` (optional extraction)
- Test: `crates/llmux-core/tests/*` existing proxy tests remain green; add `crates/llmux-server/tests/server_contract.rs` smoke for passthrough vs conversion

**Interfaces:**
- Consumes: Tasks 1-3 `protocol` + `adapters::build_passthrough`, existing `proxy::*` converters
- Produces: For each ingress (`Chat` for `/v1/chat/completions`, `Responses` for `/v1/responses`, `Messages` for `/v1/messages`), `let target = target_protocol(ingress, DownstreamMode::from_str(&resolution.upstream_api), &account)` then `if ingress==target { passthrough } else { convert → build_passthrough(target) → back-convert }`. Aggregate: per-candidate target.

- [ ] **Step 1: Write the failing test** — `server_contract.rs` mock upstream not needed; unit-test the branching via `protocol::target_protocol` already covered. Add integration smoke that `POST /v1/chat/completions` with alias in `default` and account only `messages` still returns 200 (would require conversion path).

This test will initially skip or be marked `#[ignore]` until wiring is done; alternatively test that `is_responses_unsupported` fallback now falls back to `default_protocol` not hard-coded `chat`.

- [ ] **Step 2: Run test to verify it fails / is ignored**

Run: `~/.cargo/bin/cargo test -p llmux-server server_contract -- --nocapture`
Expected: ignored or FAIL

- [ ] **Step 3: Write minimal implementation**

In `openai.rs` `chat_completions`:
```rust
let ingress = Protocol::Chat;
let mode = DownstreamMode::from_str(res.upstream_api.as_deref().unwrap_or("default"));
// per account in dispatch loop:
let target = target_protocol(ingress, mode, &account);
if ingress == target {
  let req = build_passthrough(&account, target, &body);
} else {
  let (converted_body, back_converter) = match (ingress, target) {
    (Protocol::Chat, Protocol::Responses) => (chat_to_responses(&body, &target_model), Back::ChatFromResponses),
    (Protocol::Chat, Protocol::Messages) => (chat_to_anthropic(&body, &target_model), Back::ChatFromMessages),
    _ => unreachable!(),
  };
  // dispatch with converted_body via build_passthrough(target), then back-convert response/stream
}
```

Mirror in `anthropic.rs` `messages` with `(Messages, Chat/Responses)` and in `openai.rs` `responses` ingress with `(Responses, Chat/Messages)`.

Unify `chat_via_responses` / `messages_via_responses` into `dispatch_with_conversion`. Keep `is_responses_unsupported` but on 404/501 fallback: `let fallback = default_protocol_for(&account); build_passthrough(fallback, body_converted_to_fallback)`.

For stream, keep `ResponsesTo*Converter / OpenAISseConverter` selection by `(ingress, target)` pair.

Aggregate: in `dispatch_aggregate_*` loops, compute `target` per candidate before dispatch.

- [ ] **Step 4: Run test to verify it passes**

Run: `~/.cargo/bin/cargo test -- --nocapture` (full suite)
Expected: PASS (existing proxy tests + new smoke)

- [ ] **Step 5: Commit**

```bash
git add crates/llmux-server/src/routes/v1/openai.rs crates/llmux-server/src/routes/v1/anthropic.rs crates/llmux-server/src/routes/v1/helpers.rs
git commit -m "feat(server): converge v1 ingresses on protocol target table with per-candidate dispatch"
```

---

### Task 7: Frontend — Account 三端点配置

**Files:**
- Modify: `ui/src/routes/accounts.tsx`
- Modify: `ui/src/stores/accounts.ts`
- Modify: `ui/src/lib/api.ts` (if typed)

**Interfaces:**
- Consumes: Task 4 API contract, Task 2 `GET /api/accounts` new fields
- Produces: Account create/edit form with 3 `EndpointRow` + `default_protocol` selector, local dedup, validation.

- [ ] **Step 1: Write the failing test** — manual QA checklist (no unit test infra for UI): open `/accounts`, create account, leave all unchecked → submit blocked with inline error.

- [ ] **Step 2: Run test to verify it fails** — open UI, confirm old form still shows `provider` dropdown (pre-change).

- [ ] **Step 3: Write minimal implementation** — `accounts.tsx`

Remove `provider` dropdown (new accounts hard-code `provider_id='custom'`; edit shows read-only badge). Add component:

```tsx
function EndpointRow({ label, enabled, url, urls, onToggle, onChange }: { label: string, enabled: boolean, url: string, urls: string[], onToggle: (v:boolean)=>void, onChange: (v:string)=>void }) {
  return (
    <div className="space-y-1.5">
      <label className="flex items-center gap-2 cursor-pointer">
        <input type="checkbox" checked={enabled} onChange={e=>onToggle(e.target.checked)} className="w-4 h-4 rounded accent-primary" />
        <span className="text-xs font-bold uppercase">{label}</span>
      </label>
      {enabled && (
        <div className="flex gap-2">
          <input list={`${label}-urls`} value={url} onChange={e=>onChange(e.target.value)} placeholder="https://api.example.com/v1" className="flex-1 h-9 px-3 rounded-md border border-input bg-background text-sm font-mono" />
          <datalist id={`${label}-urls`}>{urls.map(u=> <option key={u} value={u} />)}</datalist>
        </div>
      )}
    </div>
  );
}
```

In `Accounts` component, derive `distinctUrls = useMemo(() => [...new Set(accounts.flatMap(a=>[a.chat_endpoint,a.responses_endpoint,a.messages_endpoint].filter(Boolean)))], [accounts])` and split per protocol.

`formData` shape becomes `{ alias, api_key, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol }`. `default_protocol` is a `ToggleGroup`/`Select` with options filtered to enabled set; disabled until at least one enabled. Submit validates `enabled.length>0` and `default_protocol` in enabled; shows inline `text-destructive`.

`useAccountsStore`: `addAccount` / `updateAccount` send new fields; `fetchAccounts` maps new fields.

- [ ] **Step 4: Run test to verify it passes** — `npm --prefix ui run build` + manual open `/accounts` and verify 3 checkboxes, datalist, validation.

Run: `npm --prefix /home/nnb/projects/llmux/src/ui run build`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add ui/src/routes/accounts.tsx ui/src/stores/accounts.ts ui/src/lib/api.ts
git commit -m "feat(ui): account multi-endpoint form with default_protocol"
```

---

### Task 8: Frontend — Alias 下游模式与过滤

**Files:**
- Modify: `ui/src/routes/models.tsx`
- Modify: `ui/src/stores/models.ts`
- Modify: `ui/src/lib/api.ts`

**Interfaces:**
- Consumes: Task 5 API contract, Task 2 account fields
- Produces: Alias forms with `downstreamMode` (`default/chat/responses/messages`), account pickers filtered by `supports(protocol)`.

- [ ] **Step 1: Write the failing test** — manual: create alias with `forced responses` and select account that only has `chat_endpoint` → should be filtered out / 409.

- [ ] **Step 2: Run test to verify it fails** — open `/models`, confirm old `upstreamApi` options still show `auto`.

- [ ] **Step 3: Write minimal implementation** — `models.tsx`

Rename `aliasForm.upstreamApi` → `downstreamMode` (keep API field `upstream_api` for wire compat, map `default→default`). Options:
```tsx
<select value={aliasForm.downstreamMode} onChange={e=>setAliasForm({...aliasForm, downstreamMode:e.target.value})}>
  <option value="default">默认（尽可能透传）</option>
  <option value="chat">强制 Chat</option>
  <option value="responses">强制 Responses</option>
  <option value="messages">强制 Messages</option>
</select>
```

Filter `matchingAccounts`:
```tsx
const supports = (acc:any, proto:string) => proto==='chat' ? !!acc.chat_endpoint : proto==='responses' ? !!acc.responses_endpoint : !!acc.messages_endpoint;
const filtered = aliasForm.downstreamMode==='default' ? matchingAccounts : matchingAccounts.filter(a=>supports(a, aliasForm.downstreamMode));
```

On submit, if `downstreamMode !== 'default'` and selectedIds contains unsupported, show `409` toast and highlight.

Aggregate modal: same filtering per candidate row; on `downstreamMode` change, re-validate each candidate's account.

`stores/models.ts`: `addAlias` / `saveAggregateAlias` send `upstream_api: downstreamMode` (with `default` mapping).

- [ ] **Step 4: Run test to verify it passes**

Run: `npm --prefix /home/nnb/projects/llmux/src/ui run build` + manual `/models` flow
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add ui/src/routes/models.tsx ui/src/stores/models.ts ui/src/lib/api.ts
git commit -m "feat(ui): alias downstream mode with protocol-aware account filtering"
```

---

### Task 9: E2E 与灰度验证

**Files:**
- Modify: none (verification only)
- Test: manual `curl` / `cargo test` + staging deploy

**Interfaces:**
- Consumes: Tasks 1-8

- [ ] **Step 1: Write the failing test** — define E2E script `scripts/e2e_protocol_split.sh` (or inline python) that:
  1) creates account with 3 identical endpoints + default chat,
  2) creates `of-default/default`, `of-chat/chat`, `of-resp/responses` aliases,
  3) hits each alias via `chat`, `responses`, `messages` ingress and asserts 200 + `usage` non-zero,
  4) clears `responses_endpoint` and asserts `of-resp` fell back to `default` then `responses`→`default` conversion still 200.

Pre-change: script fails on step 4 (no fallback).

- [ ] **Step 2: Run test to verify it fails**

Run: `bash scripts/e2e_protocol_split.sh`
Expected: FAIL on bulk-fallback assertion

- [ ] **Step 3: Write minimal implementation** — none, just run after Tasks 1-8 land.

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
~/.cargo/bin/cargo test -- --nocapture
~/.cargo/bin/cargo build --release && /home/nnb/projects/llmux/deploy/deploy.sh -y
bash scripts/e2e_protocol_split.sh
```
Expected: PASS, `docker ps` shows `llmux` up, `/llmux/api/health` 200, no `truncated` regressions.

- [ ] **Step 5: Commit** (if script added)

```bash
git add scripts/e2e_protocol_split.sh
git commit -m "test(e2e): protocol split passthrough and fallback verification"
```

---

## Self-Review

**Spec coverage:**
- §1.3-1.5 (Account tri-endpoint, DownstreamMode, decision table) → Tasks 1, 2, 6, 9
- §2.1 (models/adapters/protocol) → Tasks 1, 2
- §2.2 (adapters endpoint selection, probe) → Task 3
- §2.3 (proxy reuse, match ingress/target) → Task 6
- §2.4 (server three-ingress convergence, aggregate per-candidate) → Task 6
- §2.5 / §3.2 / §3.3 (frontend Account/Models, distinct URLs, affectedAliases toast) → Tasks 7, 8
- §3.1 (forced 409) → Task 5
- §3.2 (bulk fallback on endpoint removal) → Task 4
- §3.4 (auto→default) → Task 2 migration
- §3.5 (runtime is_responses_unsupported → default_protocol) → Task 6
- §3.6 (no container replacement) → Task 9 deploy constraint
- §4.x (tests & phased rollout) → Tasks 1-9 phased

**Placeholder scan:** No `TBD/TODO/xxx` remaining; each step has concrete code/commands.

**Type consistency:** `Protocol` / `DownstreamMode` / `Account.{chat,responses,messages}_endpoint/default_protocol` / `ProviderRequest` / `409 alias_protocol_unsupported` naming is consistent across tasks. Wire field `upstream_api` kept for DB compat, mapped to `DownstreamMode` in Rust/UI.

