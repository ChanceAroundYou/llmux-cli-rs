# LLMux 数据库全新设计——实施记录

> 日期：2026-08-05
> 性质：按 `docs/superpowers/specs/2026-08-05-llmux-db-redesign-design.md` 全量实施的记录，含本会话全部修改与测试内容
> 分支：preview（未提交）

---

## 1. 概述

按 spec 推倒重来数据库 schema（9 张新表取代旧 7 表），同步重写存储层、数据访问、dispatcher 路由、API 路由层与前端 UI。期间按用户反馈修复了 3 个运行时 bug、1 个性能问题，并做了 2 次架构决策调整。

**测试基线：`cargo test --workspace` 全绿（core 17 / gateway 5 / server 11，含 2026-08-06 恢复 openai_compatible 后新增的 2 条）。**

---

## 2. Schema 变更（`crates/llmux-core/src/migrations/0001_init.sql`）

| 新表 | 取代 | 说明 |
|---|---|---|
| `vendors` | `providers` | 厂商目录，`protocol ∈ (openai,anthropic,gemini,custom)`，`builtin` 标记，9 个种子厂商（openai/anthropic/gemini/deepseek/moonshot/zhipu/siliconflow/zai/huoshan），支持用户自建持久复用 |
| `accounts` | 旧 `accounts` | `vendor_id` 外键、`name`（原 alias）、`enabled`（原 is_active）、`openai_compatible`（gemini 走 OpenAI 兼容端点，2026-08-06 恢复）、`api_key_enc` AES-GCM 密文、`base_url`/`anthropic_base_url`（NULL = 用厂商默认） |
| `model_aliases` | 旧 `model_aliases` | 去掉 `account_ids`/`preferred_account_id` JSON 列，改为 `vendor_id` 外键 |
| `model_alias_accounts` | `account_ids` JSON 列 | alias↔账户绑定，`position` + `is_preferred`，`UNIQUE(alias_id,account_id)`，部分唯一索引 `uq_alias_one_preferred`（每 alias 最多一个首选） |
| `api_keys` | 旧 `api_keys` | 网关 key **明文存储**（用户决策，见 §4.4），`key TEXT UNIQUE` |
| `api_key_models` | `allowed_models` JSON 列 | key 模型白名单（空表 = 不限制），CASCADE |
| `usage_logs` | 旧 `usage_logs` | **无 token 列**；`ts`（原 timestamp）、`account_name` 写时快照、`account_id` SET NULL、无 `is_test` |
| `dispatch_state` | — | 回退粘滞状态表（**未接入持久化**，见 §6） |
| `app_settings` | `settings` | 类型化 key-value |

**连接层**（`db.rs`）：`connect_sqlite` 开启 `PRAGMA foreign_keys=ON`（CASCADE/SET NULL/删厂商被挡的前提）；删除旧迁移文件（0002/0003/0004）。

---

## 3. 实现内容

### 3.1 数据访问（`llmux-core`）

- **`models.rs`**：新结构 `Vendor` / `Account`(vendor_id/name/enabled/api_key_enc) / `AccountPublic` / `ModelAlias`(vendor_id) / `ModelAliasAccount` / `ApiKey`(key 明文) / `UsageLog`(无 token) / `UsageLogParams` / `SettingRow`。删除 `Provider` / `ModelPrice`。
- **`dispatcher.rs`**：
  - `resolve_model` 按 spec §4.3：alias 有绑定 → JOIN `model_alias_accounts` 取精确账户集（`is_preferred` 优先）；无绑定 → 按 `vendor_id` 路由；无 alias → 前缀回退（claude-→anthropic, gemini-→gemini, 其余→openai）。
  - `get_active_accounts` / `get_accounts_by_ids`：JOIN `vendors` 解析 `protocol` 与有效 base_url（`COALESCE(NULLIF(account.base_url,''), vendor.default_base_url)`），解密 `api_key_enc`。
  - adapter `Account` 带 `protocol` / `custom_base_url`（显式自定义 base_url，gemini 区分官方 x-goog-api-key 与自定义 Bearer）/ `custom_anthropic_base_url`（显式配置 Anthropic 兼容端点）。
  - `DispatchRouter`（sticky 状态机）保持不变，仍内存态。
- **`crypto.rs`**：scrypt `log_n=15 → 13`；密文版本化 `v2:{log_n}:{salt}:{nonce}:{ct}`（v1 旧密文兼容解密）。
- **`usage.rs`**：`UsageService` 无 token 聚合，`account_name` 快照，无 `is_test` 过滤。
- **`settings.rs`**：表改为 `app_settings`。
- **`export_import.rs`**：新字段（vendor_id/name/bindings）；网关 key 明文可导出/导入。

### 3.2 API 路由（`llmux-server`）

- **accounts**：`vendor_id`/`name`/`enabled`，创建时查 `vendors` 校验协议，base_url 为空自动填厂商默认；删除依赖外键级联（绑定 CASCADE、usage SET NULL）。
- **vendors**（新增 CRUD）：`GET/POST /api/vendors`、`PUT/DELETE /api/vendors/:id`（有账户引用时删除被 FK 挡 → 409）。
- **keys**：明文存储；创建返回 `id` + `key`；列表返回明文 key（一键配置用）；`allowed_models` 支持 `"*"` / JSON 数组 / 裸字符串（按单模型名，安全）。
- **aliases**：`vendor_id` + 绑定集（`account_ids`/`preferred_account_id`）在一个 POST 里事务写入。
- **usage/health/activity**：适配无 token、account_name 快照。
- **v1 处理器**（openai/anthropic/gemini）：协议过滤——openai 只收 `openai|custom` 协议；anthropic 收 `anthropic` 协议 **或** 显式 `anthropic_base_url` 的账户；gemini 只收 `gemini` 协议。
- **middleware**：网关 key 明文比对鉴权 + `api_key_models` 白名单。
- **auth**（web-session）：账户映射到 `vendors.id`（精确匹配 → 协议猜测 → 回退 openai）。

### 3.3 数据访问解耦（`crates/llmux-core/src/repo.rs`，用户要求）

路由/中间件/测试中的裸 SQL 全部收拢到 `repo.rs` 免费异步函数（`&SqlitePool` 首参）：

- **vendors**：`list_vendors` / `create_vendor` / `update_vendor` / `delete_vendor` / `get_vendor`(protocol+default_base_url)
- **accounts**：`list_accounts_public` / `create_account` / `get_account` / `update_account` / `delete_account` / `find_account_by_vendor_and_name` / `set_account_api_key_enc`
- **api_keys**：`list_api_keys` / `create_api_key` / `update_api_key_name` / `delete_api_key` / `update_api_key_last_used` / `list_key_models` / `replace_key_models` / `find_api_key_by_value`
- **model_aliases**：`list_aliases` / `get_alias_name_by_id` / `upsert_alias` / `delete_alias` / `list_alias_bindings` / `replace_alias_bindings` / `delete_key_model_by_name`
- 查询专用：`list_account_id_name` / `get_account_usage_stats` / `list_account_usage_logs` / `list_recent_activity` / `get_model_health` / `list_alias_custom_models`

重构文件：vendors / accounts / keys / models/aliases / auth / health / usage / models/health / models/available / middleware。

### 3.4 前端（`ui/`，Bun + React，经 `ui/dist` 嵌入）

- 新增 `stores/vendors.ts`（厂商目录）。
- accounts：厂商下拉选择（选中自动填 `default_base_url`）、`name`/`enabled`、`toggleEnabled`、移除 `openai_compatible`。
- keys：列表明文 key 回显（eye 切换 + 复制）、`allowed_models` 兼容数组、创建返回 id。
- Setup 面板（ClaudeCode/Codex/Gemini）：一键配置直接用 `selectedKey.key` 自动填，恢复原快速配置流程。
- models：`provider_id → vendor_id`、alias 绑定集、`account_ids` 数组。
- AliasModal/CustomAliasModal：vendor 匹配、`[{vendor_id}] {name}` 显示。

---

## 4. 会话期间的修复与用户决策

### 4.1 修复的 bug

| # | 问题 | 根因 | 修复 |
|---|---|---|---|
| 1 | 建 alias 报 `FOREIGN KEY constraint failed` | `/api/models/available` 的模型 `owned_by` 填的是**账户名**，前端当 `vendor_id` 提交 | `owned_by` 改为 `account.vendor_id`，按 vendor 去重 |
| 2 | 建 key 写白名单报 `no such table: api_keys_old` | `db.rs` 迁移里 `ALTER TABLE RENAME` 会把 `api_key_models` 外键拖到 `_old` 表，drop 后断链 | 迁移函数检测 `is_legacy`（有 key_hash 列）与 `fk_broken`（外键不指向 api_keys），断链即重建 `api_key_models` |
| 3 | `/v1/messages` 对 deepseek 返回 503 | anthropic 路由协议过滤 `protocol=="anthropic"` 排除了 openai 协议但配了 `anthropic_base_url` 的账户 | 过滤器改为「anthropic 协议 **或** `custom_anthropic_base_url`」 |
| 4 | 建 key 响应无 `id`（与 accounts 不一致） | 遗漏 | 响应补 `id` |
| 5 | `allowed_models` 裸字符串变 `*` 放行（安全） | `parse_allowed_models` 解析裸字符串 JSON 失败返回 `[]`（= 不限制） | 裸字符串按单模型名限制 |

### 4.2 性能

- **scrypt 慢**：`Params::recommended`（log_n=15）在 debug 下单次 ~8s，导致 `/test` 与每次网关请求加解密卡顿、测试 60s。改为 `log_n=13` + 密文版本化 v2（兼容旧 v1）。实测 debug 单次 ~250ms，`cargo test` core 60s → 4s。
- 运行建议：`cargo run --release`（release 下加解密 ~几十 ms）。

### 4.3 用户决策

1. **网关 key 明文存储**（推翻 spec §8 点②哈希存储）：用户原话「网关 key 就相当于各厂商 ApiKey 的集合，厂商 key 加密一下就行了，自己的无需加密」。厂商账户 key 仍 AES-GCM 加密。
2. **数据库操作解耦**：路由/测试中的裸 SQL 集中到 `repo.rs`。
3. **测试库删旧重建**：`~/.config/llmux-repair/llmux_db.db` 直接删掉重建，不做旧→新迁移；`master.key` 保留。

---

## 5. 测试内容

### 5.1 自动化测试（`cargo test --workspace`，全绿）

**`crates/llmux-core/tests/core_contract.rs`（18 个）**
- `init_db_creates_fresh_schema_and_seed_vendors`：9 表存在、旧表（providers/model_prices/settings）不存在、9 个种子厂商、usage_logs 无 token 列
- `api_key_encryption_uses_authenticated_random_ciphertext`：AES-GCM 随机密文、v2 前缀、加解密往返
- `api_key_ciphertext_is_v2_self_describing_and_round_trips`：v2 带 log_n 自描述
- `master_key_is_persisted_and_idempotent`：master.key 持久化、显式 env 优先
- `settings_service_round_trips_json_and_gateway_key`
- `usage_service_logs_minimal_rows_updates_limit_cache`：无 token、account_name 快照、summary/近期/详情查询
- `foreign_keys_enforce_vendor_and_cascade_bindings`：未知 vendor 被拒、删账户绑定 CASCADE、usage SET NULL、快照保留、删被引用厂商被挡
- `each_alias_allows_at_most_one_preferred_account`：部分唯一索引
- `resolve_model_prefers_bindings_then_vendor_then_prefix`：spec §4.3 路由语义
- `active_accounts_resolve_vendor_base_url_and_protocol`：JOIN vendors 解析 base_url/protocol/custom_base_url
- `custom_anthropic_base_url_marks_anthropic_compatible_accounts`
- `export_import_preserves_new_fields_and_regenerates_keys`：vendor_id/name/bindings、key 明文导出导入
- `model_structs_use_new_field_names`
- `migration_rebuilds_hash_api_keys_to_plaintext_and_repairs_fk`
- `migration_repairs_broken_api_key_models_fk`
- `init_db_repairs_api_key_models_fk_after_legacy_rename`

**`crates/llmux-core/tests/gateway_contract.rs`（5 个）**
- openai/custom 透传注入 Bearer 与端点、anthropic 透传改 model + 注入头、target URL v1 后缀、SSE usage 解析合并、dispatcher 排序与回退

**`crates/llmux-server/tests/server_contract.rs`（9 个）**
- 读路由空态形状（含 `/api/vendors` 种子目录）、写路由校验与形状（含 keys 创建返回 key、import）、v1 鉴权 401 占位、404、SPA 回退、system tools/claude-settings、`account_alias_binding_round_trip_and_cascade`（建账户→绑 alias→删账户级联）

### 5.2 手动验证（curl）

- 密钥鉴权：创建 key → 用 key 访问 `/v1/models` 200，错误 key 401
- 账户/alias/绑定增删改查 + 删除级联
- vendors 目录返回 9 个种子厂商
- **网关 key 回读**：创建后列表返回同一明文
- **deepseek + alias 走 `/v1/messages`**：路由通到 `api.deepseek.com/anthropic`（假 key 返回 401 证明连通）
- 数据库迁移：对用户真实断链库副本实测修复后建 key 带白名单正常

---

## 6. 已知取舍 / 待办

1. **`dispatch_state` 未接入持久化**：表已建，但 sticky router 仍是内存态（`Instant` 计时不可序列化，改持久化会动热路径）。spec 要求「重启不丢回退状态」，待后续单独做。
2. **网关 key 明文存储**：库文件泄露可读网关 key（用户决策接受；厂商 key 仍加密）。
3. **gemini 账户的 OpenAI 兼容模式（`openai_compatible`）已恢复**（2026-08-06 用户反馈该功能系特意添加、曾被我方实施时误删）：`accounts` 加回 `openai_compatible` 列；`v1/openai.rs` 过滤放行 `protocol ∈ {openai, custom}` **或** `gemini && openai_compatible`；未自定义 base_url 的 gemini 兼容账户默认端点 = 厂商默认 base + `/openai`（`effective_openai_base_url` helper，含测试覆盖）。
4. **协议路由收紧**：openai 请求只走 openai/custom 协议账户；anthropic 请求走 anthropic 协议或显式 anthropic_base_url 账户；gemini 只走 gemini。
5. 前端 `i18n/locales` 中 `setup.manualApiKey` / `keys.keyShownOnce` 键已不被引用（明文回读后不再需要），留着无害可清理。
6. `crates/llmux-server/tests/server_contract.rs` 的 `account_alias_binding` 测试耗时约 30s（含一次 scrypt 加密），属正常。

---

## 7. 运行与验证

```bash
# 测试环境（.env 指向 25999 + ~/.config/llmux-repair）
cargo run                # debug
cargo run --release      # 推荐：加解密快

# 全量测试
cargo test --workspace
```

线上库（`~/.config/llmux`）未动；切换线上按 spec §10.4 流程，需先备份、改 `.env`、停旧起新。
