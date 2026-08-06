# 别名返回形状改进 + OpenAPI 文档集成（Design）

> 日期：2026-08-06
> 性质：展示层/返回层改进 + 引入 API 文档工具链。**数据库 schema 不动、路由语义不动。**
> 前置背景：2026-08-05 数据库重设计已提交（97c4e24），工作区另有未提交的 openai_compatible 恢复改动，两者独立，本次改动与之共存。

---

## 1. 目标

1. 前端拿到别名数据时能**直接看清绑定了哪些厂商、厂商有哪些属性**，不再拿 `account_ids` 数字去拼三层查询。
2. 引入 **OpenAPI（utoipa）** 工具链：后端自动生成 `openapi.json` + Swagger UI，前端用 `openapi-typescript` 生成 TS 类型，接口字段在编辑器里一目了然。

## 2. 范围

### 做

- `GET /api/models/aliases` 返回形状增强（新增厂商聚合字段，保留现有字段）
- 前端别名列表按厂商展示、别名弹窗按厂商分组
- utoipa + Swagger UI 集成，核心接口标注
- 前端生成 openapi 类型并提交

### 不做

- ❌ 数据库 schema 变更（保留 `model_alias_accounts` 账户绑定、`model_aliases.vendor_id`）
- ❌ dispatcher 路由语义变更（仍是「alias 有绑定 → 精确账户集」）
- ❌ 厂商级绑定（方案 A 已搁置）
- ❌ 手动维护 OpenAPI YAML

---

## 3. alias 返回形状（方案 B）

### 3.1 目标响应

`GET /api/models/aliases` 每项改为：

```json
{
  "id": 1,
  "alias": "gml5.2",
  "target_model": "gml-5.2",
  "vendor_id": null,
  "created_at": "2026-08-06 10:00:00",
  "accounts": [
    { "id": 1, "name": "ZaiMain", "vendor_id": "zai", "vendor_name": "阶跃星辰 StepFun", "protocol": "openai", "is_preferred": true },
    { "id": 2, "name": "HsMain", "vendor_id": "huoshan", "vendor_name": "火山方舟 Ark", "protocol": "openai", "is_preferred": false }
  ],
  "preferred_account_id": 1
}
```

- **`accounts`（替代 `account_ids`）**：绑定账户数组，每个账户带 id / 账户名 / 厂商 id / 厂商名 / 协议 / 首选标记。前端拿到直接渲染，数字与账户一一对应，无需再查。
- **保留** `preferred_account_id`：由 `is_preferred` 推导，供前端编辑首选下拉。
- 不再返回 `vendors` 聚合 / `preferred_vendor`（绑定底子是账户，厂商信息随账户给出）。

### 3.2 响应结构体（server 侧定义，derive `Serialize` + `ToSchema`）

```rust
pub struct AliasAccountSummary {
    pub id: i64,
    pub name: String,
    pub vendor_id: String,
    pub vendor_name: String,
    pub protocol: String,
    pub is_preferred: bool,
}

pub struct AliasResponse {
    pub id: i64,
    pub alias: String,
    pub target_model: String,
    pub vendor_id: Option<String>,
    pub created_at: Option<String>,
    pub accounts: Vec<AliasAccountSummary>,
    pub preferred_account_id: Option<i64>,
}
```

> 结构体放 **server crate**（带 `utoipa::ToSchema`），避免 core 依赖 utoipa。

### 3.3 数据查询（repo）

新增批量查询，一次取所有 alias 的绑定账户 + 厂商信息，**消除当前每 alias 一次查询的 N+1**：

```sql
SELECT b.alias_id, a.id AS account_id, a.vendor_id, v.name AS vendor_name, v.protocol, b.is_preferred
FROM model_alias_accounts b
JOIN accounts a ON a.id = b.account_id
JOIN vendors v ON v.id = a.vendor_id
ORDER BY b.alias_id, b.position, b.id
```

内存中按 `alias_id` 分组 → 聚合出 `vendors` 数组与 `preferred_vendor`。

---

## 4. 前端展示

### 4.1 别名列表（models.tsx）

每项显示绑定账户的 **chips**：`[zai] ZaiMain`（厂商 + 账户名），首选账户（`is_preferred`）高亮并加「首选」标记；无绑定显示「前缀回退」标记。账户名/厂商名直接来自返回的 `accounts` 数组。

### 4.2 别名弹窗（AliasModal.tsx）

账户勾选列表**按厂商分组**：组头显示厂商名 + 该组账户数；勾选仍提交 `account_ids`（底层不变）。编辑回显从返回的 `accounts` 数组取已勾选账户 id（`accounts.map(a => a.id)`）。

### 4.3 类型与 i18n

- `stores/models.ts`：`ModelAlias` 接口加 `accounts: AliasAccountSummary[]`（替代 `account_ids`/`vendors`）
- zh/en.json：首选提示、前缀回退文案

---

## 5. utoipa 集成

### 5.1 依赖

`crates/llmux-server/Cargo.toml` 加：

- `utoipa`（含 derive 特性）
- `utoipa-swagger-ui`

版本与 workspace 的 axum 主版本匹配。

### 5.2 OpenApi 挂载

- 新增 `crates/llmux-server/src/api_docs.rs`：

```rust
#[derive(OpenApi)]
#[openapi(
    paths(accounts::list_accounts, /* ...全部核心接口 */),
    components(schemas(AliasResponse, AliasVendorSummary, /* 复用现有 struct */))
)]
pub struct ApiDoc;
```

- `app.rs` 挂载：`.merge(SwaggerUi::new("/swagger").url("/api-docs/openapi.json", ApiDoc::openapi()))`
- 文档地址：`http://localhost:<port>/swagger`；规范地址 `/api-docs/openapi.json`

### 5.3 依赖边界：core 结构体进 components

`Vendor` / `AccountPublic` / `ApiKey` 等结构体定义在 `llmux-core`。要作为 OpenAPI schema 出现在 `components` 里，需实现 `utoipa::ToSchema`。两种做法：

- **给 `llmux-core` 加 `utoipa` 依赖（仅 `derive` 特性）**，在现有结构体上 `derive(ToSchema)`——推荐，改动最小。
- 或在 server 侧重定义 API DTO 并转换——工作量大，不推荐。

采用前者。`AliasResponse` / `AliasVendorSummary` 属 server 侧新类型，直接在 server 定义并 derive。

### 5.4 标注策略（控制工作量）

| 优先级 | 接口 | 标注方式 |
|---|---|---|
| 核心 | `/api/accounts`、`/api/vendors`、`/api/keys`、`/api/models/aliases`、`/api/models/available`、`/api/models/health`、`/api/usage/*`、`/api/activity`、`/api/settings` | `#[utoapi::path]` 完整标注；响应 schema **复用现有结构体**（`AccountPublic`/`Vendor`/`ApiKey`/`ModelAlias` 等 derive `ToSchema`），新增 `AliasResponse`/`AliasVendorSummary` |
| 透传 | `/v1/chat/completions`、`/v1/responses`、`/v1/messages`、`/v1beta/*`、`/v1/models` | 标路径 + 简述，响应宽松描述 |
| 次要 | `/api/health`、`/api/system/*`、`/api/auth/*`、`/api/export`、`/api/import` | 标路径即可 |

### 5.5 前端类型生成

```bash
npx openapi-typescript http://localhost:25975/api-docs/openapi.json -o ui/src/api/openapi.d.ts
```

生成的 `.d.ts` **提交进仓库**，前端直接 import 类型使用。

---

## 6. 涉及文件

**后端**
- `crates/llmux-server/Cargo.toml`（依赖）
- `crates/llmux-core/src/repo.rs`（批量查询 `list_alias_bindings_with_vendors`）
- `crates/llmux-server/src/routes/models/aliases.rs`（返回形状 + 响应结构体）
- `crates/llmux-server/src/api_docs.rs`（新）
- `crates/llmux-server/src/app.rs`（挂载 Swagger）
- 各核心 route 文件（标注）
- `crates/llmux-server/tests/server_contract.rs`（测试）

**前端**
- `ui/src/stores/models.ts`（类型）
- `ui/src/routes/models.tsx`（列表 chips）
- `ui/src/components/Models/AliasModal.tsx`（按厂商分组）
- `ui/src/i18n/locales/{zh,en}.json`
- `ui/src/api/openapi.d.ts`（生成并提交）
- `ui/package.json`（devDep `openapi-typescript`）

---

## 7. 测试

- `server_contract.rs`：alias 返回形状断言——`accounts` 数组的账户 id / 账户名 / 厂商名 / protocol / `is_preferred` 正确；`preferred_account_id` 由首选账户推导；无绑定别名 `accounts` 为空、`preferred_account_id` 为 null
- 现有测试保持通过

---

## 8. 已知取舍

- 绑定信息以 `accounts` 账户数组返回（每个账户带厂商信息）；`preferred_account_id` 由首选账户推导，供编辑回显。
- utoipa 标注是一次性工作，后续新增接口需标注才会进文档。
- 前端类型通过「后端运行 → 生成 → 提交」获得；后端接口变更后需重新生成。
- 本次与工作区未提交的 openai_compatible 恢复改动共存。

---

## 9. vendors 多协议声明（2026-08-06 追加）

背景：第三方厂商普遍同时支持多种协议（如 deepseek 官方提供 Anthropic 兼容端点 `api.deepseek.com/anthropic`），vendors 钉死单一 `protocol` 与事实不符。

### 9.1 建模

- `vendors.protocol`（保留）：主协议，路由默认 / account.protocol 来源，运行时逻辑不变。
- `vendors.protocols`（新增）：支持协议全集（JSON 数组，如 `["openai","anthropic"]`），`TEXT NOT NULL DEFAULT '["openai"]'`。
- 内置种子：`deepseek → ["openai","anthropic"]`；其余内置厂商默认 `[主协议]`（不确定支持的协议不编造，可自建/编辑补全）。

### 9.2 API 与前端

- `GET /api/vendors` 返回 `protocols: string[]`；创建/更新接收 `protocols`（JSON 数组或逗号串，缺省 `[主协议]`，只保留合法协议并确保主协议在列表中）。
- 前端建账户时「启用 Anthropic 协议端点」选项改为：**厂商 `protocols` 含 `anthropic` 或主协议为 `anthropic`** 才显示（替换旧的 `protocol ∈ {openai, custom}` 判断）。

### 9.3 openai_responses（OpenAI Responses API 能力，2026-08-06）

OpenAI 协议分 `chat/completions` 与 `responses` 两种调用，两者**共用 `default_base_url`**（仅调用端点不同，不像 anthropic 有独立 base_url）；部分第三方兼容厂商未实现 Responses API。

- `vendors.openai_responses INTEGER NOT NULL DEFAULT 1`：是否支持 `/v1/responses`。
- 内置种子：openai=1（官方支持）；第三方 openai 兼容厂商（deepseek/moonshot/zhipu/siliconflow/zai/huoshan）保守=0（多数仅 chat，确认后可在 API 改 1）。anthropic/gemini 主协议的厂商该字段无意义。
- 路由：`openai_dispatch` 对 `endpoint == "responses"` 的请求额外过滤 `account.openai_responses == true`；`Account` 经 `ACCOUNT_SELECT` JOIN `vendors.openai_responses`。
- 自定义厂商：目前仅 API 层支持（`openai_responses` 字段），前端厂商管理 UI 用户明确后续重构时一并做（暂不加）。

### 9.4 迁移

- 测试库**删旧重建**（不写 ALTER 迁移）：新 schema 直接建 `protocols` / `openai_responses` 列。用户已确认接受旧测试数据丢失。
