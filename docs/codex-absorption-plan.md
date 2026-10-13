# Codex 订阅上游吸收方案（sidecar → llmux 原生）

> 长期目标：把 `openai-api-server-via-codex` 的核心能力吸收进 llmux，去掉旁挂容器。
> 本文只覆盖**前两块**（令牌生命周期 + codex 私有请求头）。第三块（请求体翻译）
> 继续留在 sidecar，见「为什么不吸翻译层」。

## 现状

| 组件 | 位置 | 职责 |
|---|---|---|
| `codex-api` 容器 | 路由器 `llmux_default` 网络，`:18080` | 把 ChatGPT Plus 订阅的 Codex 额度转成 OpenAI Responses/Chat API |
| llmux 账户 | `provider_id=codex`，`base_url=http://codex-api:18080/v1`，`balance_provider=none` | 纯透传，不做协议转换 |
| 凭据 | 路由器 `./codex-auth/auth.json`（独立 device-auth 会话，UID 1000） | sidecar 自己读、自己刷新 |

sidecar 的 Go 源码（`internal/app/`）拆成三块，职责清晰：

| 文件 | 职责 | 吸收？ |
|---|---|---|
| `auth.go` | 读 `auth.json` → JWT 过期就 refresh → 写回文件 | ✅ 吸 |
| `backend.go` | 设置 codex 私有头，打 `chatgpt.com/backend-api/codex` | ✅ 吸 |
| `compat.go` | Responses ↔ Chat 请求体翻译 | ❌ 留 sidecar |

## 要吸的两块

### 块 1：OAuth 令牌生命周期

**sidecar 现在怎么做的**（`auth.go`）：
- `authProvider.borrow`：stat `auth.json`，按 size/mtime/freshness 缓存；要求 `auth_mode=chatgpt` 且 `tokens.access_token` 非空。
- `authProvider.refresh`：JWT 过期时 POST `https://auth.openai.com/oauth/token`，`grant_type=refresh_token`，带 `client_id` 和存储的 `refresh_token`；响应更新 `access_token`/`refresh_token`/`id_token`，写临时文件再 rename 覆盖。
- `accountID`：从 `account_id` 或 JWT claims 推导。

**llmux 要做的**：
- `accounts` 表加一个字段存 auth.json 三要素（`access_token`/`refresh_token`/`id_token`），复用现有 `api_key` 字段存 `access_token` 即可——但 `refresh_token`/`id_token` 需要新字段。
- 请求前若 JWT 过期（或收到 401），跑一次 refresh，把新 `access_token` 写回 DB。
- **为什么吸这块**：令牌过期是codex 账户最常见的故障源，现在靠 sidecar 在容器里静默刷新，llmux 完全看不到。吸进来后 llmux 自己管令牌，sidecar 就可以退休了。

### 块 2：codex 私有请求头

**sidecar 现在怎么做的**（`backend.go`）：
- `originator: openai-api-server-via-codex`
- `ChatGPT-Account-Id`（从 JWT 解出）
- `OpenAI-Beta: responses=experimental`（流式）
- `session_id` / `x-client-request-id`（有 request-id 时）
- 后端 base：`https://chatgpt.com/backend-api/codex`

**llmux 要做的**：
- `build_passthrough_with_beta()`（`adapters/mod.rs:412`）里给 `provider_id=codex` 的账户注入这些头。
- `ChatGPT-Account-Id` 复用 `balance.rs` 已有的 `jwt_chatgpt_account_id()`（`balance.rs:264`）。
- `originator` 值可以改成 `llmux` 或保留原值，看后端是否校验。
- **为什么吸这块**：这几个头是 codex 后端的硬要求，少一个就 401。吸进来后 llmux 直连 `chatgpt.com/backend-api/codex`，不再需要 sidecar 做中转。

## 为什么不吸翻译层

`compat.go` 做的是 Codex Responses 私有 payload ↔ OpenAI Responses API 的翻译：
- `chatToResponse` / `responseToChat`：消息格式互转。
- `normalizeResponseInput` / `ensureResponse`：补字段、清 input。

**不吸的理由**：
1. **私有协议，变动成本高**。ChatGPT 后端改一次 payload，sidecar 跟一次发版；吸进 llmux 就是主项目跟着发版。
2. **sidecar 的整个存在价值就是吸收这些变更**。把它搬进 llmux 等于把运维负担从上游项目搬到自己仓库。
3. **风险隔离**。sidecar 挂了只影响 codex 账户；吸进 llmux 后翻译层出 bug 会影响整个网关。

## 建议的实施路径

分两步走，每步都能独立验证：

### 步骤 A：llmux 直连 codex（去掉 sidecar）

前提：llmux 能直接打 `chatgpt.com/backend-api/codex`，自己管令牌和私有头。

1. **迁移 0031**：`accounts` 表加 `codex_auth_json`（TEXT，存 auth.json 三要素的 JSON）。
2. **新模块 `crates/llmux-core/src/codex_auth.rs`**：
   - `load(account) -> CodexCreds`：读 `codex_auth_json`，解析三要素。
   - `refresh_if_needed(creds) -> Result<CodexCreds>`：JWT 过期就 POST refresh，返回新 creds。
   - `account_id(creds) -> String`：复用 `jwt_chatgpt_account_id()`。
   - 写回 DB 用现有 `update_account` 路径。
3. **改 `build_passthrough_with_beta()`**：
   - `provider_id=codex` 时，base 直接用 `https://chatgpt.com/backend-api/codex`，不走 `endpoint_for()`。
   - 注入 `originator` / `ChatGPT-Account-Id` / `OpenAI-Beta` 头。
   - 请求前 `refresh_if_needed`，401 时 retry 一次。
4. **验证**：codex 账户直连 `chatgpt.com`，模型别名指向它，流式/非流式都通。
5. **回退**：保留 sidecar 作为 fallback，账户 `base_url` 改回 sidecar 地址即可。

### 步骤 B（可选）：吸翻译层

只有当 sidecar 项目停更、或你确定要彻底去掉 Go 依赖时才做。

1. 把 `compat.go` 的 `chatToResponse` / `responseToChat` 翻译成 Rust。
2. 放进 `proxy/responses.rs` 或新模块。
3. **风险最高**，建议永远别做，除非 sidecar 真的挂了。

## 故障隔离设计

吸收时别丢 sidecar 架构的隔离性：

- `provider_id=codex` 的账户出问题，**只禁用这些账户**，llmux 其他上游不受影响。
- 前端账户页给 codex 账户一个明显的标记（`provider_id=codex` 已够）。
- 如果 codex 后端封了私有协议，只需把 codex 账户 `is_active=0`，llmux 继续服务其他上游。

## 风险与缓解

| 风险 | 缓解 |
|---|---|
| codex 后端改私有协议 | 只影响 codex 账户；sidecar 保留为 fallback，随时切回 |
| 令牌 refresh 失败（refresh_token 过期） | 前端明确报错「codex 凭据失效，请重新登录」，不静默失败 |
| 请求体翻译层出 bug | 不吸翻译层，永远让 sidecar 做 |
| 账号风控（私有后端 + 共享订阅） | 这是 sidecar 方案自带的，吸收不改变风险等级；必要时切回 sidecar |

## 验证清单（步骤 A 完成后）

- [ ] codex 账户直连 `chatgpt.com/backend-api/codex`，不经过 sidecar
- [ ] 令牌过期时 llmux 自动 refresh，无人工干预
- [ ] 401 时 retry 一次（refresh 后重发）
- [ ] 流式 / 非流式都正常（复用刚修的 `resolve_event_type`）
- [ ] 前端账户页能看到 codex 账户的令牌状态（过期时间）
- [ ] sidecar 容器可以停掉，codex 账户不受影响

## 相关文件

- `crates/llmux-core/src/adapters/mod.rs:412` — `build_passthrough_with_beta()`
- `crates/llmux-core/src/balance.rs:264` — `jwt_chatgpt_account_id()`（已有，复用）
- `crates/llmux-core/src/protocol.rs:61` — `endpoint_for()`（codex 账户要绕过）
- `crates/llmux-core/src/migrations/0031_*.sql` — 新迁移（待建）
- `crates/llmux-server/src/routes/system/codex.rs` — `codex_auth_path()`（读 auth.json 的设置页，可复用路径逻辑）
- sidecar 源码：`internal/app/auth.go` / `backend.go` / `compat.go`
