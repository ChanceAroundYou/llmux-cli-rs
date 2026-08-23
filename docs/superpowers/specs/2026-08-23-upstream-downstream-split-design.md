# 上下游拆分：Account 多端点 + Alias 下游路由模式 — 设计文档

- 日期：2026-08-23
- 状态：Draft（Section 1/2 已确认，Section 3/4 已定版待实现）
- 决策：方案 A（三列 `chat_endpoint / responses_endpoint / messages_endpoint + default_protocol`，共享单 `api_key`）

## 0. 背景与目标

llmux 是 API 聚合网关，链路分 3 段：`用户 → llmux → 上游 Provider`。多数 `account` 同时暴露 `chat/completions`、`responses`、`messages` 三种协议；历史模型里 `provider_id + base_url / anthropic_base_url` 决定出口、`model_aliases.upstream_api` 决定别名到上游的翻译分支，能力声明分散在别名而非账户，导致跨协议路由与翻译难以统一。

目标：把架构显式拆为**上游（Account 能力）**与**下游（Alias 路由意图）**两层，统一多端点建模与路由决策，前端可配置，后端可迁移、可回滚，不替换现有容器/数据卷与部署形态。

非目标：新增第 4 协议、改计费/限额模型、动 `DispatchRouter` 粘滞策略。

---

## 1. 架构与数据流（已确认）

### 1.1 现状锚点

- 上游：`accounts { base_url, anthropic_base_url, provider_id }` 决定 `POST {base}/chat/completions` vs `{base}/v1/messages`；`upstream_api` 却在 `model_aliases / aggregate_aliases` 上，account 本身无协议能力声明。
- 下游：`v1/chat_completions | v1/responses | v1/messages` 三入口各自查 `alias.upstream_api` 决定是否走 `chat→responses / messages→responses` 翻译，分支分散在 `openai.rs / anthropic.rs`。

### 1.2 新分层

```
下游 (alias 视角)                上游 (account 视角)
┌─────────────────┐             ┌──────────────────────────┐
│ alias.downstream│  ingress P  │ account:                  │
│ = default|chat  │ ─────────► │  chat_endpoint       NULL? │
│   |responses    │  选 target  │  responses_endpoint  NULL? │
│   |messages     │  协议 + 端点 │  messages_endpoint   NULL? │
└─────────────────┘             │  default_protocol ∈ {…}  │
                                └──────────────────────────┘
User P_ingress → llmux (选 P_target) → 上游 P_target 端点 (按需 proxy 转换)
```

### 1.3 上游 Account 新形态（DB）

```sql
ALTER TABLE accounts ADD COLUMN chat_endpoint TEXT;
ALTER TABLE accounts ADD COLUMN responses_endpoint TEXT;
ALTER TABLE accounts ADD COLUMN messages_endpoint TEXT;
ALTER TABLE accounts ADD COLUMN default_protocol TEXT DEFAULT 'chat'
  CHECK (default_protocol IN ('chat','responses','messages'));
-- 回填（一次性，见 4.1）；应用层再校验：
-- default_protocol 对应列 IS NOT NULL；至少一列非 NULL
```

- `provider_id` 保留仅作展示/分组标签，新建 account 统一写 `custom`，前端不再让用户选。
- `api_key` 仍单列共享，三端点复用同一 key。
- 保留 `base_url / anthropic_base_url` 列只读做回滚锚点，新代码不再写入它们（写入新三列时可同步清空旧列或保持最后一次写入以便旧二进制回滚可读；推荐保持不变，零风险）。

### 1.4 下游 Alias 新形态

- `model_aliases.upstream_api` / `aggregate_aliases.upstream_api` 复用为 `downstream_mode ENUM('default','chat','responses','messages')`，`auto` 迁移到 `default`。
- `resolve_model → ModelResolution { downstream_mode, provider_id, target_model, account_ids… }`（字段重命名，值域不变）。

### 1.5 `P_ingress → P_target` 决策表（核心）

| alias.downstream | P_ingress | account 支持集 | P_target | 动作 |
|---|---|---|---|---|
| `default` | chat/resp/msg | 含 P_ingress | P_ingress | 直通该 endpoint，无转换 |
| `default` | chat/resp/msg | 不含 P_ingress | default_protocol | 转到 default 端点 + 走 proxy 转换 |
| `chat` / `responses` / `messages` | 任意 | 必须含该值，否则建 alias 时拒绝 | 固定为该值 | 一律转到该协议（`ingress→target` 经 proxy 转换），即使 ingress 已是同协议也走该端点 |
| 上游取消勾选某协议 | — | 受影响 alias | — | 批量 `UPDATE alias SET downstream_mode='default' WHERE downstream_mode='<被删协议>'` |

> 强制模式不做二次兜底：下游强制 responses + 上游仅 messages → 创建期直接 409，前端下拉已过滤不可选；若上游事后删除该端点则批量回落到 default。

### 1.6 请求流（stream / non-stream 统一）

1. 鉴权 → `resolve_model / resolve_aggregate` 拿 `downstream_mode` + 候选 `account_ids`
2. 选 `P_target`（查表）→ 从 `account.P_target_endpoint` 取 URL（`chat→/chat/completions`, `responses→/responses`, `messages→/v1/messages`，`adapters::build_*_passthrough` 复用）
3. 若 `P_ingress == P_target` → passthrough 加 `Authorization` 直发
4. 否则 → `proxy::{chat↔responses, messages↔responses, chat↔messages}` 转换 body（含 `to_responses_tools` 已有逻辑），发 `P_target`，回程再逆转换 + `ResponsesTo*Converter / OpenAISseConverter` 流式回译
5. `DispatchRouter` 仍按 `alias:xxx` 粘滞选 account（不变），`spawn_log_usage` 记录真实 `P_target` usage

### 1.7 聚合别名

候选 `candidates[i]` 各自可能异构（不同 account 的 `default_protocol` 不同）。`default` 模式下逐候选按上表各自算 `P_target` 再试；强制模式下创建期已保证所有候选都支持该协议，运行时无需再判断。

---

## 2. 组件与接口（已确认）

### 2.1 DB / Core 模型

- 新增 migration `0009_account_endpoints.sql`（见 1.3）。
- `crates/llmux-core/src/models.rs::Account` 新增四字段；`AccountPublic` 同步（脱敏后供 `GET /api/accounts` 返回三端点与 `default_protocol`）。
- `adapters::Account` 同步四字段；`get_active_accounts / get_accounts_by_ids` 的 `SELECT` 显式加入新列（避免 `SELECT *` 缺列）。
- `ModelAlias.upstream_api` / `AggregateAlias.upstream_api` 在 Rust 侧重命名为 `downstream_mode`（DB 列名保持 `upstream_api` 仅在 Rust 侧做类型别名，避免重建表）。`UpstreamApi::Auto` 标记 `#[deprecated]`，`from_str("auto")` → `Chat` 并在读时视为 `default`，写时不再产出 `auto`。
- 新增 `crates/llmux-core/src/protocol.rs`（或复用 `upstream_api.rs` 重命名为 `protocol.rs`）：

```rust
enum Protocol { Chat, Responses, Messages }
fn target_protocol(ingress: Protocol, downstream_mode: DownstreamMode, account: &Account) -> Protocol
fn endpoint_for(account: &Account, p: Protocol) -> Option<&str>
fn supports(account: &Account, p: Protocol) -> bool // endpoint.is_some()
```

纯函数，无 I/O，便于单测覆盖决策表。

### 2.2 Adapters：端点选择

- `build_openai_passthrough` / `build_anthropic_passthrough` 保留，新增 `build_passthrough(account, protocol, body) -> ProviderRequest`：按 `protocol` 选 `chat_endpoint / responses_endpoint / messages_endpoint` 拼 URL（`chat→/chat/completions`, `responses→/responses`, `messages→/v1/messages`），`Authorization: Bearer {api_key}` 共享。
- `normalize_base_url` 复用；`test_provider_connection` 按 `default_protocol` 的端点探测（而非 `provider_id` 分支）。

### 2.3 Proxy：复用现有转换器

- 已有 `proxy::responses::{chat↔responses, anthropic↔responses}` 与 `proxy::{anthropic_openai, openai_anthropic}` 直接复用，不新增转换器；`protocol::target_protocol` 决定是否需要转换以及走哪一对。
- 流式回译沿用 `ResponsesToChatConverter / ResponsesToAnthropicConverter / OpenAISseConverter`，仅调用点由单一 `if upstream_api.wants_responses()` 分支改为 `match (ingress, target)`。

### 2.4 Server 三入口分支（最小改动）

- `app.rs` 路由不变：`POST /v1/chat/completions` / `/v1/responses` / `/v1/messages`。
- 每入口统一流程：
  1. `resolve_model / resolve_aggregate` 拿 `downstream_mode` + 候选 accounts
  2. `let target = protocol::target_protocol(ingress, mode, &account)`（aggregate 逐候选算各自 target）
  3. `if ingress == target { passthrough } else { proxy::convert → dispatch → proxy::back_convert }`
- `openai.rs::chat_via_responses` / `anthropic.rs::messages_via_responses` 不再作为独立“别名驱动”的分支，收敛为通用 `dispatch_with_conversion(ingress, target)`；保留 `is_responses_unsupported` 仅用于 `target==Responses` 但上游实际 404 时的回退（回退到 account 的 `default_protocol`，而非固定 chat）。
- 聚合别名：`dispatch_aggregate_*` 保持 `V-anchored` 逻辑，`default` 模式下每候选独立 `target`；强制模式下创建期已保证同质，无需运行时再滤。

### 2.5 前端

- `ui/src/routes/accounts.tsx`：`provider` 下拉移除（新建固定 `custom`，编辑仅展示标签）；新增三行 `EndpointRow { checkbox enabled, combobox url }`：
  - 未勾选：`endpoint = null`，输入禁用
  - 勾选：`Select` 下拉 = 去重 URL（前端从 `accounts` 列表本地去重，避免新增接口），支持手输；失焦校验 URL 合法
  - `default_protocol`：`ToggleGroup`/`Select`，选项 = 已勾选集合，必选其一；提交前校验至少勾一、default 在集合内
- `ui/src/routes/models.tsx`：`aliasForm.upstreamApi` 重命名为 `downstreamMode`，选项改为 `默认 / 强制 Chat / 强制 Responses / 强制 Messages`（值 `default/chat/responses/messages`）；创建/编辑时：
  - `default`：账户多选不过滤
  - 强制某协议：账户多选列表过滤为 `account.supports(protocol)` 的子集；若已选账户中有不支持的，提交前 409 提示并高亮
  - 聚合别名同理，每行候选的账户下拉按 `downstreamMode` 过滤
- `ui/src/stores/accounts.ts / models.ts`：请求体字段同步新四列；`GET /api/accounts` 返回新字段供过滤与去重。
- API 契约：
  - `POST/PUT /api/accounts` 接受 `{chat_endpoint?, responses_endpoint?, messages_endpoint?, default_protocol}`，至少一端点非空，`default_protocol` 必在已启用集合内，否则 `400 { field, message }`
  - `PUT /api/accounts/:id` 删除某端点（置 null）时，服务端批量 `UPDATE model_aliases SET upstream_api='default' WHERE upstream_api='<deleted>' AND (account_ids 包含该 account 或 provider_id 匹配)`，聚合别名同理；返回 `{ affectedAliases }` 供前端 toast
  - `GET /api/accounts` / `GET /api/models/aliases` 返回新字段

---

## 3. 异常与回落

### 3.1 强制校验（创建/更新时拒绝，不在请求时兜底）

- `POST /api/models/aliases`、`PUT /api/models/aliases/:id`、`POST/PUT /api/aggregate-aliases`：若 `downstream_mode != 'default'`，校验 `account_ids` 指向的每个 account 均 `supports(downstream_mode)`；否则 `409 { code: "alias_protocol_unsupported", field: "downstream_mode", message, unsupportedAccounts: [...] }`。前端已在下拉过滤，服务端再做最终校验，防并发/直接调 API 绕过。
- `provider_id` 兜底别名（`account_ids` 为空、仅靠 `provider_id` 匹配）：同样校验该 `provider_id` 下所有 `is_active=1` 的 account 是否都支持 `downstream_mode`，任一不支持即拒绝，提示改为绑定明确 `account_ids` 或切 `default`。

### 3.2 上游删端点 → 别名批量回落

触发点：`PUT /api/accounts/:id` 将某 `*_endpoint` 置 `NULL`（取消勾选）。

事务（`BEGIN IMMEDIATE`）：

```sql
UPDATE accounts SET <proto>_endpoint = NULL, default_protocol = CASE WHEN default_protocol='<proto>' THEN <fallback> ELSE default_protocol END WHERE id=?;
-- fallback = 剩余已启用中的优先 chat > messages > responses
UPDATE model_aliases SET upstream_api='default'
 WHERE upstream_api='<proto>' AND (
   account_ids LIKE '%<id>%' OR (account_ids IS NULL AND provider_id = (SELECT provider_id FROM accounts WHERE id=?))
 );
UPDATE aggregate_aliases SET upstream_api='default'
 WHERE upstream_api='<proto>' AND EXISTS (
   SELECT 1 FROM json_each(aggregate_aliases.candidates) WHERE json_extract(value,'$.account_id') = ?
 );
```

- 仅回落 `downstream_mode == '<proto>'` 的别名；`default` 不动。
- 返回 `{ affectedAliases: { ordinary: [...alias], aggregate: [...alias] }, newDefaultProtocol }`，前端 toast 告知用户“xx 别名已回落到默认”。
- 若删除后 account 零端点，拒绝 `400 { code: "account_no_endpoint" }`，要求至少保留一个。

### 3.3 异构聚合候选

- `default` 模式：运行时逐候选独立 `target = target_protocol(ingress, 'default', candidateAccount)`，`V-anchored` 仍按原有 `V` 顺序试错，异构不影响重试语义。
- 强制模式：创建期已保证所有候选同质支持，无需运行时再判断；若后续某候选 account 被删端点，触发 3.2 批量回落后该聚合别名整体变 `default`，下次请求自动按异构路径走。

### 3.4 Auto 老值迁移

- Migration 0009 末尾：`UPDATE model_aliases SET upstream_api='default' WHERE upstream_api='auto'; UPDATE aggregate_aliases SET upstream_api='default' WHERE upstream_api='auto';`
- Rust `UpstreamApi::from_str("auto")` 保留兼容读作 `Default`，写路径永不产出 `auto`（校验层拒绝 `auto`）。
- 前端不再展示 `Auto` 选项，已存 `auto` 的别名在下次读取时显示为 `默认`。

### 3.5 运行时回退（上游实际不支持 target）

- `is_responses_unsupported` 等探测保留：若 `target == Responses` 但上游返回 `404/501/405 + is_responses_unsupported`，回退到 `account.default_protocol` 对应端点并做二次转换（仅此一种运行时回退，区别于强制模式的“不做二次兜底”——运行时回退仅发生在 `default` 模式或探测到上游谎称支持时）。

### 3.6 部署与容器约束

- **不替换现有容器/数据卷**：沿用 `deploy/deploy.sh` 流程（`cargo build --release → scp llmux → docker compose build && up -d`），`--no-recreate` 语义由 compose 保证，不做 `docker run -v /opt/...` 类覆盖；`DATA_DIR`、`/data/llmux/llmux_db.db` 与 `master.key` 铁律不变。
- 回滚：新三列为 `ADD COLUMN`，旧 `base_url / anthropic_base_url` 保留，旧二进制可回滚读取；`upstream_api` 列名不变，旧二进制将 `default` 视作 `chat` 不会崩。

---

## 4. 测试与分阶段落地

### 4.1 Migration 回填（一次性 SQL，见 1.3 展开）

```sql
UPDATE accounts SET
  chat_endpoint     = CASE WHEN provider_id IN ('openai','gemini','custom') THEN base_url END,
  messages_endpoint = CASE WHEN provider_id IN ('anthropic','custom-anthropic') THEN COALESCE(anthropic_base_url, base_url)
                           WHEN provider_id='custom' AND anthropic_base_url IS NOT NULL THEN anthropic_base_url END,
  responses_endpoint = NULL,
  default_protocol  = CASE WHEN chat_endpoint IS NOT NULL THEN 'chat'
                           WHEN messages_endpoint IS NOT NULL THEN 'messages' ELSE 'chat' END;
```

### 4.2 单元/合约测试

- `protocol::target_protocol` 决策表全覆盖（`3 ingress × 2 mode × 3 account 能力` 组合，见 `protocol.rs` 单测）。
- `POST/PUT /api/accounts` 校验：零端点拒绝、`default_protocol` 非法拒绝、删端点批量回落计数。
- `POST /api/models/aliases` 强制校验：不支持即 409。
- Proxy 转换器复用现有 `proxy::responses` 单测，不新增转换器；仅新增 `ingress==target` 直通分支的冒烟。

### 4.3 E2E（staging / 本地）

- 单 account 三端点同 URL，手建 `of(default) / of-chat(强制chat) / of-resp(强制responses)` 三别名，分别以 `chat / responses / messages` 三入口各发一次，断言均 200 且 `usage` 非零。
- 删 `responses_endpoint` 后断言 `of-resp` 自动变 `default`，再发 `responses` 入口走 `default_protocol` 转换仍 200。

### 4.4 分阶段落地

1. **Core + Migration**（`llmux-core`：models/adapters/protocol/migrations + 单测）
2. **Server 路由收敛**（`llmux-server`：三入口 `match (ingress,target)` + 聚合 + 批量回落事务）
3. **UI**（accounts 三勾选 + models 下游模式 + 过滤/校验）
4. **E2E 与灰度**（先单 account 验证，再全量切 `provider_id=custom`）

---

## 5. 风险与回滚

- 不同上游对 `responses` 的 event 序列实现不完整，已有 `response.completed.response.output` 回填作兼容保障。
- 变更集中在协议适配与路由选择；如 staging 回归，可通过 `deploy.sh` 自动保存的二进制备份回滚，或单提交 revert 源码（migration 仅 `ADD COLUMN`，回滚无需 `DROP COLUMN`）。

## 6. 附：不做事项

- 不引入第 4 协议、不改计费/限额、不重做 `DispatchRouter` 粘滞策略。
- `distinct endpoints` 去重由前端本地完成，不新增后端接口。

---

## 7. 关键文件清单（实现时先读）

- `crates/llmux-core/src/models.rs`、`db.rs`、`migrations/*`、`upstream_api.rs`（将变为 `protocol.rs`）、`adapters/mod.rs`、`dispatcher.rs`、`proxy/{mod,anthropic_openai,openai_anthropic,responses}.rs`
- `crates/llmux-server/src/app.rs`、`routes/v1/{anthropic,openai}.rs`、`routes/accounts.rs`、`routes/models/*`
- `ui/src/routes/accounts.tsx`、`ui/src/routes/models.tsx`、`ui/src/stores/{accounts,models}.ts`、`ui/src/lib/api.ts`
