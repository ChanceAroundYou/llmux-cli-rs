# LLMux API 返回字段参考（UI 对齐用）

> 来源：Rust handler 实际返回结构，非 openapi.d.ts 推断。`?` = 可空字段。
> 路径来源目录：`crates/llmux-server/src/`（路由）；`crates/llmux-core/src/`（模型/导出结构）。

---

## 账户（routes/accounts.rs）

### `GET /api/accounts`
**返回** `200: AccountPublic[]`（不含 api_key 密文）
- `id`: number ?（DB 行 id，通常存在）
- `vendor_id`: string — 厂商 id
- `name`: string — 账户名
- `base_url`: string ?
- `anthropic_base_url`: string ?
- `openai_compatible`: number（0/1）
- `enabled`: number（0/1）
- `weight`: number
- `notes`: string ?
- `created_at`: string ?

**错误** 500：错误格式见文末。

### `POST /api/accounts`
**入参**（必填：`vendor_id`/`name`/`api_key`）
- `vendor_id`: string 必填
- `name`: string 必填
- `api_key`: string 必填（明文，服务端加密存储）
- `base_url`: string ?（空则用厂商默认）
- `anthropic_base_url`: string ?
- `enabled`: number ?（缺省 1）
- `weight`: number ?（缺省 1）
- `openai_compatible`: number ?（缺省 0）
- `notes`: string ?
- `skip_validation`: boolean ?（缺省 false；true 跳过厂商模型校验）

**返回** `200`
- `success`: boolean（true）
- `id`: number — 新账户 id
- `message`: string
- `modelCount`: number — 校验拉取到的模型数
- `skippedValidation`: boolean

**错误** 400（参数缺失 / 未知 vendor / 校验失败，校验失败 message 为 `accounts.validationFailed`）；500。

### `PUT /api/accounts/{id}`
**入参**：与 POST 相同但全部可选（合并更新，缺省保留现有值）。特殊约定：
- `api_key`: string 若传 `"********"` 或空表示不改密文；传新值则重新加密并触发校验
- `base_url` 一旦出现（即使同值）即视为变更并触发校验
- `skip_validation`: boolean ?（缺省 false）

**返回** `200`
- `success`: boolean（true）
- `message`: string（"Account updated successfully"）

**错误** 404（账户不存在）；400（未知 vendor / 校验失败）；500。

### `DELETE /api/accounts/{id}`
**返回** `200`
- `success`: boolean（true）
- `message`: string（"Account deleted; bindings removed, history retained (snapshot kept)"）

**错误** 404（不存在）；500。

### `GET /api/accounts/{id}/export`
**返回** `200: text/csv 附件`（非 JSON）
- Content-Type: `text/csv; charset=utf-8`
- Content-Disposition: `attachment; filename="usage_history_account_{id}.csv"`
- 表头：`Timestamp,Model,Latency (ms),Status,Error`
- 每行：`ts, model, latency_ms, Success|Failed, error`（字段按 RFC 4180 转义）

**错误** 500。

---

## 网关 Key（routes/keys.rs）

> ⚠️ 注意：openapi.d.ts 的 `ApiKey` schema **没有** `allowed_models`，但 `GET /api/keys` 实际返回**有** `allowed_models`（见下）。UI 应补充该字段。

### `GET /api/keys`
**返回** `200: ApiKey[]`（每个对象由 handler 用 json! 手工构造）
- `id`: number ?
- `name`: string
- `key`: string — 网关 key 明文
- `enabled`: number（0/1）
- `last_used_at`: string ?
- `created_at`: string ?
- `allowed_models`: string | string[] — **白名单**。无白名单时为字符串 `"*"`（表示不限）；有白名单时为模型名数组

**错误** 500。

### `POST /api/keys`
**入参**
- `name`: string ?（缺省 "Unnamed Key"）
- `allowed_models`: string | string[] ?（缺省 `"*"`）。`"*"` = 不限；裸字符串 = 限定为单个模型名；JSON 数组字符串或数组 = 多个模型名

**返回** `200`
- `success`: boolean（true）
- `id`: number
- `key`: string — 新 key 明文（`sk-llmux-{uuid}`）

**错误** 500。

### `PUT /api/keys/{id}`
**入参**
- `name`: string ?（缺省 "Untitled Key"）
- `allowed_models`: string | string[] | null ?（`null` = 不更新白名单，避免意外清空）

**返回** `200`
- `success`: boolean（true）

**错误** 500。

### `DELETE /api/keys/{id}`
**返回** `200`
- `success`: boolean（true）

**错误** 500。
（与 Bun 一致：key 不存在也静默成功。）

---

## 模型别名（routes/models/aliases.rs）

### `GET /api/models/aliases`
**返回** `200: AliasResponse[]`
- `id`: number
- `alias`: string
- `target_model`: string
- `vendor_id`: string ?
- `created_at`: string ?
- `preferred_account_id`: number ?（= accounts 里 is_preferred 为 true 的账户 id）
- `accounts`: AliasAccountSummary[]
  - `id`: number
  - `name`: string
  - `vendor_id`: string
  - `vendor_name`: string
  - `protocol`: string
  - `is_preferred`: boolean

**错误** 500。

### `POST /api/models/aliases`
**入参**（必填：`alias`/`target_model`）
- `alias`: string 必填
- `target_model`: string 必填
- `vendor_id`: string ?
- `account_ids`: number[] | string ?（逗号串如 `"1,5"`）
- `preferred_account_id`: number ?

**返回** `200`
- `success`: boolean（true）
- `message`: string（"Alias set successfully"）

**错误** 400（缺 alias/target_model）；500。

### `DELETE /api/models/aliases/{id}`
**返回** `200`
- `success`: boolean（true）
- `message`: string（"Alias deleted successfully"）

**错误** 404（不存在）；500。

---

## 可用模型（routes/models/available.rs）

### `GET /api/models/available`
**查询**：`?force=true` 强制刷新并绕过缓存。

**返回** `200`
- `data`: 模型对象数组（异构，来自各上游 + alias 自定义模型合并）
- `stale`: boolean — 缓存是否过期
- `cached_at`: number — unix 秒

**`data` 内每个模型对象**（已 normalize 的公共字段，其余上游字段原样透传）：
- `id`: string — 取上游 `id`，或 Gemini `name` 去掉 `models/` 前缀；空则 ""
- `name`: string — 显示名：上游 `displayName` → `name`（去 `models/` 前缀）→ 回退 `id`
- `object`: string（缺省 `"model"`）
- `created`: number（缺省 0）
- `owned_by`: string — **由网关插入**，值为提供模型的厂商 id（vendor_id）
- `error`: string ? — **仅占位对象**出现：账户模型拉取失败时生成 `{ "id": "{account}-models-unavailable", "name": "{account}", "object": "model", "created": 0, "owned_by": vendor_id, "error": 错误信息 }`
- alias 自定义模型对象：`{ "id": target_model, "object": "model", "created": 0, "owned_by": vendor_id 或 "custom" }`
- 上游透传字段：OpenAI/Anthropic/Custom 的原 `data[]` 项、Gemini 的原 `models[]` 项中的额外字段（如 openai 的 `owned_by`、gemini 的 `description`/`supportedGenerationMethods` 等）都会被保留

**错误**：无显式错误分支（上游全挂时返回空数组 data）。

---

## 模型测试与健康（routes/models/testing.rs + health.rs）

### `GET /api/models/health`（routes/models/health.rs）
**返回** `200: 数组`
- `account_id`: number
- `vendor_id`: string ?
- `model`: string（null 时 ""）
- `last_checked`: number — unix 秒
- `success`: number（0/1）
- `latency`: number — 毫秒
- `error`: string ?
- `limits_cache`: object | null — 账户 limits_cache JSON 列解析结果
- `limits_cache_updated_at`: string ?
- `account_name`: string ?

**错误** 500。

### `POST /api/models/test`
**入参**
- `model`: string 必填
- `vendorId`: string ?（覆盖模型解析结果）
- `accountId`: number ?（指定测试某账户）

**返回** `200`（无论成功失败都 200，用 success 区分）
- `success`: boolean
- `latency`: number — 毫秒
- `status`: number — 上游 HTTP 状态码
- `response`: object | null — 上游响应体（失败时为 null）
- `error`: string | null

**错误** 400（缺 model）；500（模型解析失败）。注意：无活跃账户时返回 `{ "success": false, "error": "No active account found for vendor {vendor}" }`（200）。

### `POST /api/models/test-all`
**入参**
- `models`: 数组，每项 `{ "model": string, "vendorId"?: string }`

**返回** `200`
- `success`: boolean（true）
- `message`: string（"Queue started"）
- `total`: number — 队列模型数

**错误** 400（models 非数组）；409（队列已在运行，内联返回 `{ "error": "A test queue is already running." }`，非 simple_error 格式）。

### `GET /api/models/test-queue/status`
**返回** `200`
- `isRunning`: boolean
- `total`: number
- `current`: number
- `progress`: number（0–100）

**错误**：无。

---

## 厂商（routes/vendors.rs；Vendor 结构在 crates/llmux-core/src/models.rs）

### `GET /api/vendors`
**返回** `200: Vendor[]`
- `id`: string
- `name`: string
- `protocol`: string — 主协议（openai/anthropic/gemini/custom）
- `protocols`: string[] — 支持的全部协议
- `openai_responses`: boolean — 是否支持 OpenAI Responses API
- `default_base_url`: string ?
- `default_anthropic_url`: string ?
- `coding_plan`: number（0/1）— 火山方舟等 coding plan 套餐开关
- `coding_base_url`: string ?
- `coding_anthropic_url`: string ?
- `builtin`: number（0/1）— 内置种子厂商
- `created_at`: string ?

**错误** 500。

### `POST /api/vendors`
**入参**（必填：`id`/`name`）
- `id`: string 必填
- `name`: string 必填
- `protocol`: string ?（缺省 "openai"，合法值 openai/anthropic/gemini/custom）
- `default_base_url`: string ?
- `default_anthropic_url`: string ?
- `protocols`: string[] | string ?（逗号串；缺省为 `[主协议]`，主协议保证在列表中）
- `openai_responses`: boolean ?（缺省 true）
- `coding_plan`: number ?（缺省 0）
- `coding_base_url`: string ?
- `coding_anthropic_url`: string ?

**返回** `200`
- `success`: boolean（true）
- `message`: string（"Vendor created"）

**错误** 400（缺 id/name / protocol 非法）；500。

### `PUT /api/vendors/{id}`
**入参**：与 POST 相同，全部可选（合并更新）。`default_base_url`/`default_anthropic_url`/`coding_base_url`/`coding_anthropic_url` 显式传 `null` = 清空。
**返回** `200`
- `success`: boolean（true）
- `message`: string（"Vendor updated"）

**错误** 404（不存在）；400（protocol 非法）；500。

### `DELETE /api/vendors/{id}`
**返回** `200`
- `success`: boolean（true）
- `message`: string（"Vendor deleted"）

**错误** 404（不存在）；409（仍有账户引用，报 "still referenced by accounts"）；500。

---

## 活动（routes/usage.rs）

### `GET /api/activity`
**查询**：`?limit=50`（缺省 50，最大 200）。

**返回** `200`
- `entries`: 数组
  - `id`: number
  - `timestamp`: number — unix 秒
  - `model`: string（null 时 ""）
  - `success`: number（0/1）
  - `latency_ms`: number
  - `error_message`: string ?
  - `account_name`: string ?
- `totalRequests`: number — **全表**总请求数（非 entries 窗口内）
- `successCount`: number — **全表**成功数

**错误** 500。

---

## 设置（routes/settings.rs）

### `GET /api/settings`
**返回** `200: 平铺键值对象`
- 任意 `key: value`，value 为 app_settings 表 value 列的 JSON 解析结果；解析失败则原样字符串；NULL 为 `null`。常见键如 `gateway_key`（网关访问凭据）。

**错误** 500。

### `PUT /api/settings`
**入参**：平铺 JSON 对象（任意 `key → value`，value 可以是字符串/数字/布尔/对象，非字符串会 JSON 序列化后存储）。
**返回** `200`
- `success`: boolean（true）（空对象也返回 `{ "success": true }`）

**错误** 400（body 非对象）；500。

### `POST /api/settings/reset`
**返回** `200`
- `success`: boolean（true）
- `message`: string（"Database purged successfully"）

**说明**：清空 usage_logs / model_alias_accounts / api_key_models / api_keys / model_aliases / accounts；保留 vendors / dispatch_state / app_settings（含 gateway_key）。
**错误** 500。

---

## 健康检查（routes/health.rs）

### `GET /api/health`
**返回** `200: 数组`（每个账户一条）
- `id`: string（格式 `acc_{id}`）
- `name`: string — 账户名
- `status`: string — `healthy` / `degraded` / `down` / `unknown`（按最近请求成功率：>0.9 healthy，>0.5 degraded，否则 down；无请求 unknown）
- `lastSuccess`: number — 成功数
- `totalChecks`: number — 总请求数

**错误** 500。

---

## 系统工具检测（routes/system/mod.rs）

### `GET /api/system/tools`
**返回** `200`
- `vscode`: boolean
- `claude`: boolean
- `gemini`: boolean
- `opencode`: boolean
- `codex`: boolean

**错误**：无。

---

## CLI 配置读写（routes/system/claude.rs / codex.rs / gemini.rs）

### `GET /api/system/claude-settings`
**返回** `200`
- `exists`: boolean — settings.json 是否存在
- `settings`: object | null — settings.json 解析结果（不存在/解析失败为 null）
- `error`: string ? — 仅在读取失败或无法确定 HOME 时出现

### `POST /api/system/claude-settings`
**入参**
- `apiBaseUrl`: string 必填
- `apiKey`: string 必填
- `opusModel`: string ?
- `sonnetModel`: string ?
- `haikuModel`: string ?

**返回** `200`
- `success`: boolean（true）
- `backupPath`: string ?（首次写入无备份时为 null）
- `settings`: object — 合并后的 settings.json（含 `env` 段：`ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN`/`ANTHROPIC_DEFAULT_*_MODEL` 等）

**错误** 400（缺 apiBaseUrl/apiKey，内联 `{ "error": ... }`）；500（内联 `{ "success": false, "error": ... }`）。

### `GET /api/system/codex-settings`
**返回** `200`
- `exists`: boolean
- `auth`: object | null — auth.json 解析结果
- `configToml`: string | null — config.toml 原文

### `POST /api/system/codex-settings`
**入参**
- `apiBaseUrl`: string 必填
- `apiKey`: string 必填
- `model`: string ?（缺省 "gpt-5.4"）
- `reviewModel`: string ?（缺省 = model）
- `wireApi`: string ?（缺省 "responses"）
- `contextWindow`: number ?
- `autoCompactLimit`: number ?

**返回** `200`
- `success`: boolean（true）
- `backupPath`: string
- `settings`: object
  - `auth`: object（`{ "OPENAI_API_KEY": key }`）
  - `configToml`: string — 生成的 config.toml 内容

**错误** 400（缺必填）；500（内联 `{ "success": false, "error": ... }`）。

### `GET /api/system/gemini-settings`
**返回** `200`
- `exists`: boolean
- `env`: string | null — .env 原文
- `settings`: string | null — settings.json 原文（注意：字符串，非对象）

### `POST /api/system/gemini-settings`
**入参**
- `apiKey`: string 必填
- `gatewayUrl`: string 必填
- `model`: string ?（缺省 "gemini-3-pro-preview"）

**返回** `200`
- `success`: boolean（true）
- `backupPath`: string
- `settings`: object
  - `env`: string — 写入的 .env 内容（含 `GEMINI_API_KEY`/`GOOGLE_GEMINI_BASE_URL`）
  - `settings`: string — 写入的 settings.json 内容（字符串）

**错误** 400（缺必填）；500（内联 `{ "success": false, "error": ... }`）。

---

## CLI 配置备份（routes/system/{claude,codex,gemini}.rs）

三个工具的备份接口结构一致（claude 备份前缀 `settings.json.`，codex 前缀 `codex.`，gemini 前缀 `gemini.`）。

### `GET /api/system/{tool}-backups`
**查询**：`?name=xxx` 可选。
- 带 `?name=`：读取单个备份 → **返回** `200: { "settings": object }`（备份文件 JSON 解析结果）
- 不带 `?name=`：**返回** `200: 数组`（按 name 降序，新的在前）
  - `name`: string
  - `path`: string
  - `timestamp`: string — 本地时间 `YYYY-MM-DD HH:MM:SS`
  - `size`: number — 字节
  - 备份目录不存在时返回 `[]`

**错误** 400（name 非法：非前缀 / 含 `/` 或 `..`）；404（备份不存在）；500。

### `POST /api/system/{tool}-backups`
**入参**
- `name`: string 必填（备份文件名）

**返回** `200`
- claude：`{ "success": true, "settings": object }`
- codex / gemini：`{ "success": true }`

**错误** 400（缺 name / name 非法）；404（备份不存在）；500。

### `DELETE /api/system/{tool}-backups`
**入参**
- `name`: string 必填（备份文件名）

**返回** `200`
- `success`: boolean（true）

**错误** 400（缺 name / name 非法）；404（备份不存在）；500。

---

## 全量配置导出/导入（crates/llmux-core/src/export_import.rs，路由在 routes/settings.rs）

### `GET /api/export`
**返回** `200: application/json 附件`（Content-Disposition: `attachment; filename="llmux-config-{毫秒时间戳}.json"`）。body 为 ConfigExport：
- `version`: number（当前 2）
- `accounts`: 数组
  - `id`: number
  - `vendor_id`: string
  - `name`: string
  - `api_key`: string — **解密后明文**
  - `base_url`: string ?
  - `anthropic_base_url`: string ?
  - `openai_compatible`: number
  - `enabled`: number
  - `weight`: number
  - `notes`: string ?
- `aliases`: 数组
  - `alias`: string
  - `target_model`: string
  - `vendor_id`: string ?
  - `account_ids`: number[]
  - `preferred_account_id`: number ?
- `keys`: 数组
  - `name`: string
  - `key`: string — 网关 key 明文
  - `allowed_models`: string[]（白名单，可为空数组）
- `settings`: 数组（**不含** `gateway_key`）
  - `key`: string
  - `value`: string

**错误** 500。

### `POST /api/import`
**入参**：ConfigExport 结构（见 GET /api/export）。反序列化兼容旧版字段：account 的 `provider_id`→`vendor_id`、`alias`→`name`、`is_active`→`enabled`。

**返回** `200`
- `success`: boolean（true）
- `imported`: object
  - `accounts`: number
  - `aliases`: number
  - `keys`: number

**错误** 400（配置格式无效）；500。

---

## Web Session（routes/auth.rs）

### `POST /api/auth/web-session`
**入参**
- `token`: string 必填（非空）
- `provider`: string 必填（≤64 位 `[a-zA-Z0-9_-]`；非已注册 vendor 时自动创建独立 vendor）
- `alias`: string ?（缺省 `{provider}-web`）

**返回** `200`
- `success`: boolean（true）
- `message`: string（"Web Session for {provider} imported/updated successfully as {alias}"）

**错误** 400（缺 token/provider 或 provider 非法）；500。

---

## 通用错误响应格式（crates/llmux-server/src/error.rs）

- **`simple_error`**（绝大多数非 200）：`{ "error": string }`，状态码见各接口。
- **`gateway_error`**（网关统一错误）：`{ "error": { "message": string, "type": string, "code": string } }`
  - `code` 是状态码字符串（如 `"404"`）。
  - 已用于：`not_found()` → 404 `{ message: "Not Found", type: "not_found", code: "404" }`；`unauthorized_missing_key()` → 401 `{ message: "Missing API Key. Gateway is locked.", type: "authentication_error", code: "401" }`。
- **例外**（不走 helper，内联构造，结构不一致，UI 需兼容）：
  - `POST /api/models/test-all` 队列已运行时 409 → `{ "error": "A test queue is already running." }`
  - `POST /api/models/test` 无活跃账户 → 200 `{ "success": false, "error": "..." }`
  - `apply_claude_settings` / `apply_codex_settings` / `apply_gemini_settings` 的部分 400/500 → `{ "error": "..." }` 或 `{ "success": false, "error": "..." }`
  - `GET /api/system/claude-settings` 读取异常 → 200 `{ "exists": true, "settings": null, "error": "..." }`

---

## openapi.d.ts 与实际情况不一致清单

1. **`ApiKey` 缺 `allowed_models`**（最重要的差异）：openapi.d.ts 的 `components.schemas.ApiKey` 只有 `id/name/key/enabled/last_used_at/created_at`，但 `GET /api/keys` handler 实际构造的对象**包含** `allowed_models`（`"*"` 字符串或模型名数组）。`list_api_keys` 的 200 响应被类型化为 `ApiKey[]`，UI 按此访问 `allowed_models` 会报类型错误。
2. **`get_activity` 200 无 content 类型**：openapi.d.ts 中 `get_activity` 的 200 是 `content?: never`（无 body 声明），但实际返回完整 JSON `{ entries, totalRequests, successCount }`。
3. **`export_config` 200 无 content 类型**：openapi.d.ts 中 `export_config` 200 为 `content?: never`，实际返回 application/json 附件。
4. **`get_available_models` / `get_models_health` 200 无 content 类型**：openapi.d.ts 中均 `content?: never`，实际返回 JSON 对象/数组。
5. 其余已类型化接口（AccountPublic / Vendor / AliasResponse / AliasAccountSummary / ApiKey）字段与 Rust 结构一致。
