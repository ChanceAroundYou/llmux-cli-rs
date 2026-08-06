# 别名返回形状改进 + OpenAPI 文档集成 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让别名接口返回带厂商聚合信息（前端直接可渲染），并引入 utoipa + Swagger UI + 前端 TS 类型生成的 API 文档工具链。

**Architecture:** 后端在 `GET /api/models/aliases` 返回时 JOIN accounts→vendors 聚合出 `vendors` 数组（数据库不动）；新增 utoipa OpenApi 文档挂 `/swagger`，核心接口标注 `#[utoipa::path]`，复用 core 现有结构体 derive `ToSchema`；前端用 `openapi-typescript` 生成 `.d.ts` 提交。

**Tech Stack:** Rust axum 0.7 / sqlx 0.8 / utoipa 5 / utoipa-swagger-ui 8（axum 0.7 兼容版）；React + zustand + i18next；openapi-typescript。

## Global Constraints

- 数据库 schema **不动**：保留 `model_alias_accounts`（账户绑定）、`model_aliases.vendor_id`。
- dispatcher 路由语义**不动**（仍是 alias 有绑定 -> 精确账户集）。
- 保留 `account_ids` / `preferred_account_id` 字段（编辑回显需要）。
- `llmux-core` 加 `utoipa`（仅 `derive` 特性）依赖；`llmux-server` 加 `utoipa` + `utoipa-swagger-ui`。
- utoipa 版本：`5`；utoipa-swagger-ui 版本：`8`（与 axum 0.7 兼容；若编译冲突则按 cargo 报错调整到最新兼容版）。
- 测试基线：`cargo test --workspace` 全绿；`npx tsc --noEmit` 通过。
- 注释禁止第一人称；TS 禁止 `any`（用 `unknown` + 类型守卫）。
- 不主动 git commit，每个 Task 末尾的 commit 步骤仅在用户确认后执行；若用户未要求提交，跳过 commit 步骤但完成所有代码改动。

---

## File Structure

**后端**
- `crates/llmux-core/Cargo.toml` — 加 `utoipa` 依赖
- `crates/llmux-core/src/models.rs` — `Vendor`/`AccountPublic`/`ApiKey`/`ModelAlias` derive `ToSchema`
- `crates/llmux-core/src/repo.rs` — 新增 `list_alias_bindings_with_vendors` 批量查询
- `crates/llmux-server/Cargo.toml` — 加 `utoipa` + `utoipa-swagger-ui`
- `crates/llmux-server/src/api_docs.rs`（新）— OpenApi 聚合 + Swagger 挂载类型
- `crates/llmux-server/src/lib.rs` — 暴露 `api_docs` 模块
- `crates/llmux-server/src/app.rs` — `.merge(SwaggerUi)`
- `crates/llmux-server/src/routes/models/aliases.rs` — 返回形状增强 + 响应结构体 + `#[utoipa::path]`
- 各核心 route 文件 — `#[utoipa::path]` 标注（accounts/vendors/keys/models/available/health/usage/activity/settings）
- `crates/llmux-server/tests/server_contract.rs` — 返回形状测试

**前端**
- `ui/src/stores/models.ts` — `ModelAlias` 加 `vendors` / `preferred_vendor`
- `ui/src/routes/models.tsx` — 列表厂商 chips
- `ui/src/components/Models/AliasModal.tsx` — 按厂商分组勾选
- `ui/src/i18n/locales/{zh,en}.json` — 新文案
- `ui/src/api/openapi.d.ts`（生成并提交）
- `ui/package.json` — devDep `openapi-typescript`

---

## Task 1: core 加 utoipa 依赖 + 现有结构体 derive ToSchema

**Files:**
- Modify: `crates/llmux-core/Cargo.toml`
- Modify: `crates/llmux-core/src/models.rs`

**Interfaces:**
- Produces: `Vendor`/`AccountPublic`/`ApiKey`/`ModelAlias`/`SettingRow` 实现 `utoipa::ToSchema`，供 server 的 OpenApi `components` 引用。

- [ ] **Step 1: 加 utoipa 依赖到 core**

修改 `crates/llmux-core/Cargo.toml`，在 `[dependencies]` 末尾加：

```toml
utoipa = { version = "5", features = ["derive"] }
```

若 workspace 根 `Cargo.toml` 有 `[workspace.dependencies]` 集中管理，则在根加 `utoipa = { version = "5", features = ["derive"] }`，core 里写 `utoipa.workspace = true`。检查根 Cargo.toml 是否有 `[workspace.dependencies]` 段决定写法。

- [ ] **Step 2: 给现有结构体 derive ToSchema**

`crates/llmux-core/src/models.rs` 顶部 import：

```rust
use utoipa::ToSchema;
```

给以下结构体的 derive 列表加 `ToSchema`（保留现有 `Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq`）：

- `Vendor`（行 9 附近）
- `Account`（行 20 附近）
- `AccountPublic`（行 37 附近）
- `ModelAlias`（行 50 附近）
- `ApiKey`（grep `pub struct ApiKey` 定位）
- `SettingRow`（grep `pub struct SettingRow` 定位）

示例（以 `Vendor` 为例）：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq, ToSchema)]
pub struct Vendor {
    // 字段不变
}
```

- [ ] **Step 3: 编译验证**

Run: `cargo check -p llmux-core`
Expected: 通过，无错误。

- [ ] **Step 4: Commit（用户确认后）**

```bash
git add crates/llmux-core/Cargo.toml crates/llmux-core/src/models.rs
git commit -m "feat(core): derive ToSchema for shared structs"
```

---

## Task 2: repo 新增 alias 绑定批量查询（含厂商信息）

**Files:**
- Modify: `crates/llmux-core/src/repo.rs`
- Test: `crates/llmux-core/tests/core_contract.rs`

**Interfaces:**
- Produces: `pub async fn list_alias_bindings_with_vendors(pool: &SqlitePool, alias_ids: &[i64]) -> Result<HashMap<i64, Vec<AliasBindingRow>>>`
- Produces: `pub struct AliasBindingRow { pub account_id: i64, pub vendor_id: String, pub vendor_name: String, pub protocol: String, pub is_preferred: i64 }`

- [ ] **Step 1: 写失败测试**

在 `crates/llmux-core/tests/core_contract.rs` 末尾加测试（需 `use std::collections::HashMap;` 若未导入）：

```rust
#[tokio::test]
async fn list_alias_bindings_with_vendors_groups_by_alias() {
    let pool = memory_db().await;
    let secret = "vendor-agg-secret";

    // 建 zai 账户 + huoshan 账户
    let zai_acct = repo::create_account(
        &pool, "zai", "ZaiMain",
        &encrypt_api_key("sk-zai", secret).expect("encrypt"),
        None, None, 0, 1, 1, None,
    ).await.expect("insert zai account");
    let huoshan_acct = repo::create_account(
        &pool, "huoshan", "HuoshanMain",
        &encrypt_api_key("sk-hs", secret).expect("encrypt"),
        None, None, 0, 1, 1, None,
    ).await.expect("insert huoshan account");

    // 建 alias 绑定两账户，zai 为首选
    let alias_id = repo::upsert_alias(&pool, "gml52", "gml-5.2", None)
        .await.expect("upsert alias");
    repo::replace_alias_bindings(&pool, alias_id, &[zai_acct, huoshan_acct], Some(zai_acct))
        .await.expect("bind");

    let map = repo::list_alias_bindings_with_vendors(&pool, &[alias_id])
        .await.expect("query");
    let rows = map.get(&alias_id).expect("alias present");
    assert_eq!(rows.len(), 2);
    // 每行含厂商信息
    let zai_row = rows.iter().find(|r| r.vendor_id == "zai").expect("zai row");
    assert_eq!(zai_row.vendor_name, "阶跃星辰 StepFun");
    assert_eq!(zai_row.protocol, "openai");
    assert_eq!(zai_row.is_preferred, 1);
    let hs_row = rows.iter().find(|r| r.vendor_id == "huoshan").expect("huoshan row");
    assert_eq!(hs_row.is_preferred, 0);

    // 空 alias_ids 返回空 map
    let empty = repo::list_alias_bindings_with_vendors(&pool, &[]).await.expect("query");
    assert!(empty.is_empty());
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p llmux-core --lib list_alias_bindings_with_vroups 2>&1 | head`（注意：测试在 tests 目录，用 `cargo test -p llmux-core list_alias_bindings_with_vendors`）
Expected: 编译失败（函数/结构体未定义）。

- [ ] **Step 3: 实现 repo 函数与结构体**

在 `crates/llmux-core/src/repo.rs` 的 `model_aliases` 区块末尾（`replace_alias_bindings` 之后）加：

```rust
/// alias 绑定行 + 厂商信息（供返回形状聚合用）。
#[derive(Debug, Clone)]
pub struct AliasBindingRow {
    pub account_id: i64,
    pub vendor_id: String,
    pub vendor_name: String,
    pub protocol: String,
    pub is_preferred: i64,
}

/// 批量取多个 alias 的绑定账户 + 厂商信息，按 alias_id 分组。
/// 一次查询消除 N+1；alias_ids 为空时返回空 map。
pub async fn list_alias_bindings_with_vendors(
    pool: &SqlitePool,
    alias_ids: &[i64],
) -> Result<HashMap<i64, Vec<AliasBindingRow>>> {
    let mut map: HashMap<i64, Vec<AliasBindingRow>> = HashMap::new();
    if alias_ids.is_empty() {
        return Ok(map);
    }
    let placeholders: Vec<String> = alias_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT b.alias_id, b.account_id, b.is_preferred, a.vendor_id, v.name AS vendor_name, v.protocol
         FROM model_alias_accounts b
         JOIN accounts a ON a.id = b.account_id
         JOIN vendors v ON v.id = a.vendor_id
         WHERE b.alias_id IN ({})
         ORDER BY b.alias_id, b.position, b.id",
        placeholders.join(",")
    );
    let mut query = sqlx::query(&sql);
    for id in alias_ids {
        query = query.bind(id);
    }
    let rows = query.fetch_all(pool).await?;
    for row in rows {
        let alias_id: i64 = row.try_get("alias_id")?;
        let entry = map.entry(alias_id).or_default();
        entry.push(AliasBindingRow {
            account_id: row.try_get("account_id")?,
            vendor_id: row.try_get("vendor_id")?,
            vendor_name: row.try_get("vendor_name")?,
            protocol: row.try_get("protocol")?,
            is_preferred: row.try_get::<i64, _>("is_preferred").unwrap_or(0),
        });
    }
    Ok(map)
}
```

确认 `use std::collections::HashMap;` 已在 repo.rs 顶部（grep 确认；若无则加）。`Row` trait 已 import（`use sqlx::Row;`）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p llmux-core list_alias_bindings_with_vendors`
Expected: PASS。

- [ ] **Step 5: Commit（用户确认后）**

```bash
git add crates/llmux-core/src/repo.rs crates/llmux-core/tests/core_contract.rs
git commit -m "feat(core): batch alias bindings query with vendor info"
```

---

## Task 3: alias 接口返回形状增强（含厂商聚合）

**Files:**
- Modify: `crates/llmux-server/src/routes/models/aliases.rs`
- Test: `crates/llmux-server/tests/server_contract.rs`

**Interfaces:**
- Produces: `GET /api/models/aliases` 每项含 `vendors: [{vendor_id, vendor_name, protocol, account_count, has_preferred}]` + `preferred_vendor`。
- Produces（Task 4 用）: 响应结构体 `AliasResponse` / `AliasVendorSummary`（先在本 Task 用 `json!` 返回，Task 4 抽成结构体 derive ToSchema）。本 Task 暂不引入结构体，保持 `json!` 风格与现有一致。

- [ ] **Step 1: 写失败测试**

在 `crates/llmux-server/tests/server_contract.rs` 末尾加：

```rust
#[tokio::test]
async fn alias_list_returns_vendor_aggregation() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state);

    // 建 zai + huoshan 账户
    let (_, body) = request_json_shared(
        &app, Method::POST, "/api/accounts",
        Some(json!({"vendor_id": "zai", "name": "ZaiAgg", "api_key": "sk-zai", "skip_validation": true})),
    ).await;
    let zai_id = body["id"].as_i64().expect("zai id");
    let (_, body) = request_json_shared(
        &app, Method::POST, "/api/accounts",
        Some(json!({"vendor_id": "huoshan", "name": "HsAgg", "api_key": "sk-hs", "skip_validation": true})),
    ).await;
    let hs_id = body["id"].as_i64().expect("hs id");

    // 建 alias 绑两账户，zai 首选
    let (status, _) = request_json_shared(
        &app, Method::POST, "/api/models/aliases",
        Some(json!({"alias": "aggtest", "target_model": "gml-5.2", "account_ids": [zai_id, hs_id], "preferred_account_id": zai_id})),
    ).await;
    assert_eq!(status, StatusCode::OK);

    let (_, aliases) = request_json_shared(&app, Method::GET, "/api/models/aliases", None).await;
    let entry = aliases.as_array().unwrap().iter()
        .find(|a| a["alias"] == "aggtest").expect("alias exists");

    // 保留字段
    assert_eq!(entry["account_ids"], json!([zai_id, hs_id]));
    assert_eq!(entry["preferred_account_id"], zai_id);

    // 新增 vendors 聚合
    let vendors = entry["vendors"].as_array().expect("vendors array");
    assert_eq!(vendors.len(), 2);
    let zai_v = vendors.iter().find(|v| v["vendor_id"] == "zai").expect("zai vendor");
    assert_eq!(zai_v["vendor_name"], "阶跃星辰 StepFun");
    assert_eq!(zai_v["protocol"], "openai");
    assert_eq!(zai_v["account_count"], 1);
    assert_eq!(zai_v["has_preferred"], true);
    let hs_v = vendors.iter().find(|v| v["vendor_id"] == "huoshan").expect("hs vendor");
    assert_eq!(hs_v["has_preferred"], false);

    // preferred_vendor 由 preferred_account_id 推导
    assert_eq!(entry["preferred_vendor"], "zai");

    // 无绑定别名：vendors 空、preferred_vendor null
    request_json_shared(
        &app, Method::POST, "/api/models/aliases",
        Some(json!({"alias": "nobind", "target_model": "claude-3", "account_ids": []})),
    ).await;
    let (_, aliases2) = request_json_shared(&app, Method::GET, "/api/models/aliases", None).await;
    let nobind = aliases2.as_array().unwrap().iter()
        .find(|a| a["alias"] == "nobind").expect("nobind exists");
    assert_eq!(nobind["vendors"].as_array().unwrap().len(), 0);
    assert_eq!(nobind["preferred_vendor"], Value::Null);
}
```

注意：`set_model_alias` 当前对空 `account_ids` 的处理——确认能成功建别名（无绑定走前缀回退）。若当前 `replace_alias_bindings` 对空数组正常（清空后不插入），则测试通过；若报错需先修 `set_model_alias` 允许空绑定。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p llmux-server alias_list_returns_vendor_aggregation`
Expected: FAIL（`vendors` 字段不存在）。

- [ ] **Step 3: 改 aliases.rs 返回形状**

修改 `crates/llmux-server/src/routes/models/aliases.rs` 的 `get_model_aliases`。顶部加 `use llmux_core::repo;` 已有；加 `use std::collections::HashMap;`。

替换整个 `get_model_aliases` 函数为：

```rust
pub async fn get_model_aliases(Extension(state): Extension<AppState>) -> Response {
    let rows = match repo::list_aliases(&state.pool).await {
        Ok(rows) => rows,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to list aliases: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // 一次批量查询所有 alias 的绑定 + 厂商信息（消除 N+1）
    let alias_ids: Vec<i64> = rows.iter().map(|a| a.id.unwrap_or_default()).collect();
    let bindings_map = repo::list_alias_bindings_with_vendors(&state.pool, &alias_ids)
        .await
        .unwrap_or_default();

    let mut aliases = Vec::with_capacity(rows.len());
    for alias in &rows {
        let id = alias.id.unwrap_or_default();
        let bindings = bindings_map.get(&id).cloned().unwrap_or_default();

        // account_ids / preferred_account_id（保留，编辑回显用）
        let account_ids: Vec<i64> = bindings.iter().map(|b| b.account_id).collect();
        let preferred_account_id = bindings
            .iter()
            .find(|b| b.is_preferred == 1)
            .map(|b| b.account_id);

        // 厂商聚合：按 vendor_id 分组，统计 account_count、has_preferred
        let mut vendor_map: std::collections::BTreeMap<String, serde_json::Value> = std::collections::BTreeMap::new();
        for b in &bindings {
            let entry = vendor_map
                .entry(b.vendor_id.clone())
                .or_insert(json!({
                    "vendor_id": b.vendor_id,
                    "vendor_name": b.vendor_name,
                    "protocol": b.protocol,
                    "account_count": 0,
                    "has_preferred": false,
                }));
            entry["account_count"] = json!(entry["account_count"].as_i64().unwrap_or(0) + 1);
            if b.is_preferred == 1 {
                entry["has_preferred"] = json!(true);
            }
        }
        let vendors: Vec<serde_json::Value> = vendor_map.into_values().collect();

        // preferred_vendor = preferred_account_id 对应账户的 vendor_id
        let preferred_vendor = bindings
            .iter()
            .find(|b| b.is_preferred == 1)
            .map(|b| b.vendor_id.clone());

        aliases.push(json!({
            "id": id,
            "alias": alias.alias,
            "target_model": alias.target_model,
            "vendor_id": alias.vendor_id,
            "created_at": alias.created_at,
            "account_ids": account_ids,
            "preferred_account_id": preferred_account_id,
            "vendors": vendors,
            "preferred_vendor": preferred_vendor,
        }));
    }

    Json(Value::Array(aliases)).into_response()
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p llmux-server alias_list_returns_vendor_aggregation`
Expected: PASS。若 `nobind` 用例失败（空绑定建别名报错），检查 `set_model_alias` 对空 `account_ids` 的处理，必要时允许空绑定通过。

- [ ] **Step 5: 全量回归**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: 全绿（core / gateway / server）。

- [ ] **Step 6: Commit（用户确认后）**

```bash
git add crates/llmux-server/src/routes/models/aliases.rs crates/llmux-server/tests/server_contract.rs
git commit -m "feat(server): alias response includes vendor aggregation"
```

---

## Task 4: 引入 utoipa + SwaggerUi 依赖与挂载

**Files:**
- Modify: `crates/llmux-server/Cargo.toml`
- Create: `crates/llmux-server/src/api_docs.rs`
- Modify: `crates/llmux-server/src/lib.rs`
- Modify: `crates/llmux-server/src/app.rs`

**Interfaces:**
- Produces: `SwaggerUi` 挂在 `/swagger`，`openapi.json` 在 `/api-docs/openapi.json`。
- Produces: `crate::api_docs::ApiDoc`（OpenApi derive），供各 route 通过 `#[utoipa::path]` 自动收集。

- [ ] **Step 1: 加依赖**

`crates/llmux-server/Cargo.toml` 的 `[dependencies]` 加：

```toml
utoipa = { version = "5", features = ["axum_extras", "derive"] }
utoipa-swagger-ui = { version = "8", features = ["axum"] }
```

（若 workspace 根有 `[workspace.dependencies]` 集中管理 utoipa，server 用 `utoipa.workspace = true`，并补 axum_extras feature。）

- [ ] **Step 2: 创建 api_docs.rs（先空 paths，后续 Task 填充）**

`crates/llmux-server/src/api_docs.rs`：

```rust
use utoipa::OpenApi;

/// LLMux 网关 API 文档。paths/components 由各 route 的 #[utoipa::path] 与
/// core 结构体的 ToSchema 自动收集。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "LLMux Gateway API",
        version = "0.3.3",
        description = "本地单用户 AI 网关：账户、厂商、路由别名、网关鉴权、用量监控。"
    ),
    paths(),
    components(schemas())
)]
pub struct ApiDoc;
```

- [ ] **Step 3: lib.rs 暴露模块**

`crates/llmux-server/src/lib.rs` 加：

```rust
pub mod api_docs;
```

- [ ] **Step 4: app.rs 挂载 SwaggerUi**

`crates/llmux-server/src/app.rs`，顶部 import 区加：

```rust
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
use crate::api_docs::ApiDoc;
```

在 `app(state)` 函数的 `Router::new()` 链中、`.fallback(fallback)` 之前加：

```rust
        .merge(SwaggerUi::new("/swagger").url("/api-docs/openapi.json", ApiDoc::openapi()))
```

- [ ] **Step 5: 编译验证**

Run: `cargo check -p llmux-server`
Expected: 通过。若 utoipa-swagger-ui 8 与 axum 0.7 feature 不匹配，按 cargo 报错调整 feature（如 `["axum"]` -> 确认版本对应 feature 名）。

- [ ] **Step 6: 手动验证文档页可访问**

Run: `cargo run -p llmux-server`（或 `cargo run`，按 .env 测试端口 25999）
访问 `http://localhost:25999/swagger` 应出现 Swagger UI（paths 暂空属正常，Task 5 填充）。
验证后停掉进程。

- [ ] **Step 7: Commit（用户确认后）**

```bash
git add crates/llmux-server/Cargo.toml crates/llmux-server/src/api_docs.rs crates/llmux-server/src/lib.rs crates/llmux-server/src/app.rs
git commit -m "feat(server): mount utoipa Swagger UI at /swagger"
```

---

## Task 5: 核心接口 utoipa 标注（含 alias 响应结构体）

**Files:**
- Modify: `crates/llmux-server/src/routes/models/aliases.rs`
- Modify: `crates/llmux-server/src/routes/accounts.rs`、`vendors.rs`、`keys.rs`、`models/available.rs`、`models/health.rs`、`usage.rs`、`settings.rs`
- Modify: `crates/llmux-server/src/api_docs.rs`
- Test: `crates/llmux-server/tests/server_contract.rs`

**Interfaces:**
- Produces: `AliasResponse` / `AliasVendorSummary` 结构体（derive `Serialize` + `ToSchema`），`get_model_aliases` 返回它（替换 Task 3 的 `json!`）。
- Produces: 各核心 handler 加 `#[utoipa::path]`，注册进 `ApiDoc` 的 `paths(...)` 与 `components(...)`。

- [ ] **Step 1: 定义 alias 响应结构体并改 get_model_aliases 返回它**

`crates/llmux-server/src/routes/models/aliases.rs` 顶部加 import 与结构体：

```rust
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct AliasVendorSummary {
    pub vendor_id: String,
    pub vendor_name: String,
    pub protocol: String,
    pub account_count: i64,
    pub has_preferred: bool,
}

#[derive(Serialize, ToSchema)]
pub struct AliasResponse {
    pub id: i64,
    pub alias: String,
    pub target_model: String,
    pub vendor_id: Option<String>,
    pub created_at: Option<String>,
    pub account_ids: Vec<i64>,
    pub preferred_account_id: Option<i64>,
    pub vendors: Vec<AliasVendorSummary>,
    pub preferred_vendor: Option<String>,
}
```

把 Task 3 的 `get_model_aliases` 里 `aliases.push(json!({...}))` 改为构造 `AliasResponse` 结构体（字段一一对应，`vendors` 用 `AliasVendorSummary` 而非 `serde_json::Value`），最终 `Json(aliases).into_response()`。聚合逻辑不变，只是把 `json!` 换成结构体字段赋值。

- [ ] **Step 2: 给 get_model_aliases / set_model_alias / delete_model_alias 加 utoipa::path**

在 `aliases.rs` 各 handler 上加标注。示例（`get_model_aliases`）：

```rust
#[utoipa::path(
    get,
    path = "/api/models/aliases",
    responses(
        (status = 200, description = "别名列表（含厂商聚合）", body = [AliasResponse])
    )
)]
pub async fn get_model_aliases(Extension(state): Extension<AppState>) -> Response {
```

`set_model_alias`（post, path `/api/models/aliases`, request_body 含 alias/target_model/account_ids 等）、`delete_model_alias`（delete, path `/api/models/aliases/:id`）同样加标注，response 用简单 `{success: bool}` 描述。

- [ ] **Step 3: 给其他核心接口加标注**

对以下 handler 加 `#[utoipa::path]`（path / method / 简要 response body 引用 core 结构体）：

- `accounts.rs`: `list_accounts`(get `/api/accounts`, body `[AccountPublic]`)、`create_account`、`update_account`(put `/api/accounts/:id`)、`delete_account`(delete `/api/accounts/:id`)
- `vendors.rs`: `list_vendors`(get `/api/vendors`, body `[Vendor]`)、`create_vendor`、`update_vendor`、`delete_vendor`
- `keys.rs`: `list_api_keys`(get `/api/keys`, body `[ApiKey]`)、`create_api_key`、`update_api_key`、`delete_api_key`
- `models/available.rs`: `get_available_models`(get `/api/models/available`)
- `models/health.rs`: `get_models_health`(get `/api/models/health`)
- `usage.rs`: `get_activity`(get `/api/activity`)
- `settings.rs`: `export_config`(get `/api/export`)、`import_config`(post `/api/import`)

每标注需在对应文件顶部 `use utoipa::ToSchema;`（若用到 body schema）。

- [ ] **Step 4: 注册进 ApiDoc**

`crates/llmux-server/src/api_docs.rs` 的 `paths(...)` 填入所有已标注的 handler 路径（`crate::routes::accounts::list_accounts` 等），`components(schemas(...))` 填入 `AliasResponse, AliasVendorSummary` + core 的 `Vendor, AccountPublic, ApiKey, ModelAlias, SettingRow`（用全路径 `llmux_core::models::Vendor`）。

示例：

```rust
#[derive(OpenApi)]
#[openapi(
    info(...),
    paths(
        crate::routes::accounts::list_accounts,
        crate::routes::accounts::create_account,
        crate::routes::accounts::update_account,
        crate::routes::accounts::delete_account,
        crate::routes::vendors::list_vendors,
        crate::routes::vendors::create_vendor,
        crate::routes::vendors::update_vendor,
        crate::routes::vendors::delete_vendor,
        crate::routes::keys::list_api_keys,
        crate::routes::keys::create_api_key,
        crate::routes::keys::update_api_key,
        crate::routes::keys::delete_api_key,
        crate::routes::models::aliases::get_model_aliases,
        crate::routes::models::aliases::set_model_alias,
        crate::routes::models::aliases::delete_model_alias,
        crate::routes::models::available::get_available_models,
        crate::routes::models::health::get_models_health,
        crate::routes::usage::get_activity,
        crate::routes::settings::export_config,
        crate::routes::settings::import_config,
    ),
    components(schemas(
        llmux_core::models::Vendor,
        llmux_core::models::AccountPublic,
        llmux_core::models::ApiKey,
        llmux_core::models::ModelAlias,
        llmux_core::models::SettingRow,
        crate::routes::models::aliases::AliasResponse,
        crate::routes::models::aliases::AliasVendorSummary,
    ))
)]
pub struct ApiDoc;
```

注意：handler 函数须为 `pub`（grep 确认；若私有则改 pub）。`#[utoipa::path]` 标注的 path 字符串须与 app.rs 路由实际路径一致。

- [ ] **Step 5: 编译验证**

Run: `cargo check -p llmux-server`
Expected: 通过。常见错误：handler 非 pub、path 字符串不匹配、schema 未注册——按报错修。

- [ ] **Step 6: 测试回归**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: 全绿（Task 3 的 alias 形状测试仍通过，因结构体 serialize 后字段一致）。

- [ ] **Step 7: 手动验证 openapi.json 含接口**

Run: `cargo run`，`curl -s http://localhost:25999/api-docs/openapi.json | python3 -m json.tool | grep -c '"/api/'`
Expected: 输出非 0（注册的接口数）。验证后停进程。

- [ ] **Step 8: Commit（用户确认后）**

```bash
git add crates/llmux-server/src/
git commit -m "feat(server): annotate core routes with utoipa, register in OpenApi"
```

---

## Task 6: 前端类型与展示改进

**Files:**
- Modify: `ui/src/stores/models.ts`
- Modify: `ui/src/routes/models.tsx`
- Modify: `ui/src/components/Models/AliasModal.tsx`
- Modify: `ui/src/i18n/locales/zh.json`、`en.json`
- Create: `ui/src/api/openapi.d.ts`（生成）
- Modify: `ui/package.json`

**Interfaces:**
- Consumes: `GET /api/models/aliases` 新返回形状（Task 3/5）。
- Produces: 别名列表厂商 chips、弹窗按厂商分组、openapi 类型文件。

- [ ] **Step 1: stores/models.ts 加类型**

`ui/src/stores/models.ts` 的 `ModelAlias` interface 加字段：

```typescript
export interface AliasVendorSummary {
  vendor_id: string;
  vendor_name: string;
  protocol: string;
  account_count: number;
  has_preferred: boolean;
}

export interface ModelAlias {
  // 现有字段保留
  id: number;
  alias: string;
  target_model: string;
  vendor_id: string | null;
  created_at: string;
  account_ids: number[];
  preferred_account_id: number | null;
  vendors: AliasVendorSummary[];      // 新增
  preferred_vendor: string | null;    // 新增
}
```

- [ ] **Step 2: 别名列表显示厂商 chips**

`ui/src/routes/models.tsx` 的别名列表项渲染处，在 alias 名后加厂商 chips。找到别名列表渲染（grep `aliases.map` 定位），每项加：

```tsx
{alias.vendors && alias.vendors.length > 0 ? (
  <div className="flex flex-wrap gap-1 mt-1">
    {alias.vendors.map(v => (
      <span
        key={v.vendor_id}
        className={`text-[10px] px-1.5 py-0.5 rounded border ${
          v.vendor_id === alias.preferred_vendor
            ? 'border-primary/40 bg-primary/10 text-primary'
            : 'border-border bg-muted/40 text-muted-foreground'
        }`}
      >
        [{v.vendor_id}] {v.vendor_name}
        {v.vendor_id === alias.preferred_vendor && ` · ${t('models.preferred')}`}
      </span>
    ))}
  </div>
) : (
  <span className="text-[10px] text-muted-foreground/60 ml-2">{t('models.prefixFallback')}</span>
)}
```

- [ ] **Step 3: AliasModal 按厂商分组勾选**

`ui/src/components/Models/AliasModal.tsx` 的账户勾选列表（现有 `matchingAccounts.map`）改为按厂商分组。在 `matchingAccounts` 渲染前先分组：

```tsx
// 按 vendor_id 分组
const groupedByVendor = matchingAccounts.reduce<Record<string, typeof matchingAccounts>>((acc, a) => {
  (acc[a.vendor_id] ||= []).push(a);
  return acc;
}, {});
```

渲染时双层循环：外层厂商组头（显示厂商名 + 该组账户数），内层原有 checkbox。厂商名从 `safeAccounts` 的 `vendor_id` 无法直接得名——优先从传入的 `vendors`（若有）或 vendors store 取。简单做法：组头显示 `[vendor_id]`（保持与现状一致，不引入 vendors store 依赖；若 AliasModal 已有 vendors 数据则用名称）。

由于 AliasModal 当前 props 无 vendors 列表，组头先显示 `[vendor_id]`（与现有 `[a.vendor_id]` 一致），不额外加 prop，避免改动过大。分组本身已让结构更清晰。

- [ ] **Step 4: i18n 文案**

`ui/src/i18n/locales/zh.json` 的 `models` 区块加：

```json
"preferred": "首选",
"prefixFallback": "前缀回退",
```

`en.json` 对应：

```json
"preferred": "Preferred",
"prefixFallback": "Prefix fallback",
```

- [ ] **Step 5: 类型检查**

Run: `cd ui && npx tsc --noEmit`
Expected: 通过。

- [ ] **Step 6: 生成 openapi 类型并提交**

后端运行中（端口 25999）：

```bash
cd ui
npx openapi-typescript http://localhost:25999/api-docs/openapi.json -o src/api/openapi.d.ts
```

把 `openapi-typescript` 加为 devDependency：

```bash
npm install -D openapi-typescript
```

验证生成文件非空：

```bash
wc -l src/api/openapi.d.ts
```

- [ ] **Step 7: 前端类型检查 + 后端回归**

Run: `cd ui && npx tsc --noEmit`
Run: `cargo test --workspace 2>&1 | tail -10`
Expected: 前端通过、后端全绿。

- [ ] **Step 8: Commit（用户确认后）**

```bash
git add ui/src/stores/models.ts ui/src/routes/models.tsx ui/src/components/Models/AliasModal.tsx ui/src/i18n/locales/zh.json ui/src/i18n/locales/en.json ui/src/api/openapi.d.ts ui/package.json
git commit -m "feat(ui): alias vendor chips, grouped binding, openapi types"
```

---

## Self-Review 记录

- **Spec 覆盖**：alias 返回形状（Task 2/3/5）、UI chips 与分组（Task 6）、utoipa 集成（Task 4/5）、前端类型生成（Task 6）、测试（Task 2/3）均有对应 Task。
- **占位符**：无 TBD/TODO；每个 Step 含具体代码或命令。
- **类型一致**：`AliasBindingRow`（Task 2）→ `get_model_aliases` 聚合（Task 3）→ `AliasVendorSummary`/`AliasResponse`（Task 5）字段名一致（vendor_id/vendor_name/protocol/account_count/has_preferred）。
- **风险点**：utoipa-swagger-ui 8 与 axum 0.7 feature 兼容性（Task 4 Step 5 有兜底）；空绑定建别名（Task 3 Step 4 有兜底）；handler 可见性（Task 5 Step 4 提醒改 pub）。
