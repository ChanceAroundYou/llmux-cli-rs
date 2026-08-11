# LLMux UI 数据规格（Data Spec）

> **用途**：供 UI 设计师自由发挥。只给数据模型、业务能力与业务规则，**不限定任何布局/组件/导航/视觉**。
> **字段即事实**：所有字段名、类型、取值与后端实际返回一致（详见 `discuss/llmux-api-fields.md`）。

---

## 1. 产品一句话

本地 AI 网关控制台：管理上游厂商账户、配置模型路由别名、发放网关密钥、监控用量与健康，并一键把网关写入 Claude Code / Codex / Gemini 等本机 CLI 工具。

---

## 2. 业务能力清单（UI 需要满足的功能需求，不含任何布局）

> 设计师根据这些能力自主组织信息架构与界面形态。

### 2.1 账户域（Accounts）
- 查看全部账户及其状态、厂商、协议
- 添加账户（厂商 + 名称 + API Key + 可选 URL 覆盖 + 权重 + 备注）
- 编辑账户（API Key 留空=不改）、启停切换、删除（级联解除别名绑定，历史保留）
- 账户级 URL 覆盖（base_url / anthropic_base_url 覆盖厂商默认）
- 连通性校验（创建/编辑时自动拉取上游模型，可跳过）
- 导出单个账户的用量 CSV
- WebSession 导入（provider + token → 自动建账户，未知 provider 自动建独立厂商）

### 2.2 厂商域（Vendors）
- 查看厂商目录（内置种子 18 家 + 用户自定义）
- 自定义厂商（id + 名称 + 主协议 + 协议集 + 端点 URL）
- 编辑厂商（合并更新）、删除（被账户引用时拒绝 409）
- coding plan 套餐开关（开启后路由走 coding 端点）

### 2.3 模型路由域（Model Aliases）
- 查看别名及其绑定的账户集、首选账户
- 创建/编辑别名（别名名 → 目标模型 → 绑定账户集 + 首选）
- 自定义别名（任意模型名，先验证后保存）
- 删除别名（不影响同名密钥白名单）
- 模型库浏览（各上游模型目录聚合，含厂商归属）
- 单模型拨测、全量批量拨测（队列进度）

### 2.4 密钥域（API Keys）
- 查看网关密钥（掩码/明文切换）、一键复制
- 创建密钥（名称 + 模型白名单：`*` 全部 或 指定模型）
- 编辑（改白名单）、删除
- 白名单异常检测（非 `*` 但为空 → 提示重新授权）
- 展示对外 Base URL（动态取当前访问地址）

### 2.5 用量与监控域（Usage / Monitoring）
- 请求量汇总（总数/成功/失败/平均延迟）
- 随时间趋势（可切时间范围）
- 按厂商 / 按模型 / 按账户的分布
- 失败明细与回退（failover）统计
- 别名健康概览、各模型健康与 Token 限额、最近活动流
- 网关整体健康度

### 2.6 工具配置域（Setup）
- 检测本机已安装的 CLI（claude / codex / gemini / opencode / vscode）
- 为每个工具选网关密钥 + 绑模型角色，一键写入配置
- 写入前 diff 预览（改前 vs 改后）
- 配置备份管理（每次写入自动备份，可查看/还原/删除）
- WebSession 导入（浏览器脚本捕获 session token 回连）

### 2.7 系统域（Settings）
- 键值设置读写（含主题、网关 key）
- 导出全量配置 / 导入配置（含账户、别名、密钥、设置）
- 清空运行数据（保留厂商目录与网关密钥）

### 2.8 关于
- 版本、简介、开源链接、赞助入口

---

## 3. 数据模型（核心：UI 一切数据的来源）

### 3.1 Vendor 厂商

| 字段 | 类型 | 必填 | 含义 | 取值/示例 |
|------|------|------|------|-----------|
| `id` | string | ✅ | 厂商标识（创建时定，不可改） | `"deepseek"` |
| `name` | string | ✅ | 显示名 | `"DeepSeek"` |
| `protocol` | enum | ✅ | 主协议（路由默认） | `openai` / `anthropic` / `gemini` / `custom` |
| `protocols` | string[] | ✅ | 支持的全部协议 | `["openai","anthropic"]` |
| `openai_responses` | boolean | ✅ | 是否支持 OpenAI Responses API | `true`/`false` |
| `default_base_url` | string? | | OpenAI 兼容端点 | `https://api.deepseek.com` |
| `default_anthropic_url` | string? | | Anthropic 端点 | `https://api.deepseek.com/anthropic` |
| `coding_plan` | 0/1 | ✅ | coding plan 套餐开关 | 0 关 / 1 开 |
| `coding_base_url` | string? | | coding 的 OpenAI 端点 |  |
| `coding_anthropic_url` | string? | | coding 的 Anthropic 端点 |  |
| `builtin` | 0/1 | ✅ | 是否内置种子厂商 | 内置不可删除 |
| `created_at` | string? | | 创建时间 | ISO |

### 3.2 Account 账户（挂厂商下）

| 字段 | 类型 | 必填 | 含义 |
|------|------|------|------|
| `id` | number | ✅ | 账户 id |
| `vendor_id` | string | ✅ | 所属厂商 |
| `name` | string | ✅ | 账户名（唯一） |
| `api_key` | string | — | **仅创建/编辑时输入**；读接口不返回明文 |
| `base_url` | string? | | 覆盖厂商 OpenAI 端点（空=用厂商默认） |
| `anthropic_base_url` | string? | | 覆盖厂商 Anthropic 端点 |
| `openai_compatible` | 0/1 | ✅ | gemini 账户走 OpenAI 兼容端点 |
| `enabled` | 0/1 | ✅ | 启停 |
| `weight` | number | ✅ | 路由权重 |
| `notes` | string? | | 备注 |
| `limits_cache` | object? | | 上游限额缓存（Token 额度等） |
| `created_at` | string? | | 创建时间 |

### 3.3 ModelAlias 路由别名

| 字段 | 类型 | 必填 | 含义 |
|------|------|------|------|
| `id` | number | ✅ | 别名 id |
| `alias` | string | ✅ | **客户端请求用的模型名**（唯一），如 `gpt-4` |
| `target_model` | string | ✅ | 真实上游模型名 |
| `vendor_id` | string? | | 目标厂商 |
| `preferred_account_id` | number? | | 首选账户（= 绑定项里 `is_preferred` 的那个） |
| `accounts` | AliasAccountSummary[] | ✅ | 绑定账户集，每项： |
| └ `id` | number | | 账户 id |
| └ `name` | string | | 账户名 |
| └ `vendor_id` | string | | 账户厂商 |
| └ `vendor_name` | string | | 厂商显示名 |
| └ `protocol` | string | | 厂商主协议 |
| └ `is_preferred` | boolean | | 是否首选 |

### 3.4 ApiKey 网关密钥

| 字段 | 类型 | 必填 | 含义 |
|------|------|------|------|
| `id` | number | ✅ | 密钥 id |
| `name` | string | ✅ | 名称 |
| `key` | string | ✅ | **明文** `sk-llmux-{uuid}`（本地可回读） |
| `enabled` | 0/1 | ✅ | 启停 |
| `allowed_models` | `"*"` \| string[] | ✅ | 白名单：`"*"`=全部；数组=指定模型；编辑传 `null`=不改 |
| `last_used_at` | string? | | 最后使用 |
| `created_at` | string? | | 创建时间 |

> 创建响应额外含一次性明文：`{ success, id, key }`——**明文只在创建时返回一次**，UI 应设计"创建后立即展示可复制"的流程。

### 3.5 活动 / 用量

| 对象 | 字段 | 类型 | 含义 |
|------|------|------|------|
| ActivityEntry | `id` | number | |
| | `timestamp` | number | unix 秒 |
| | `model` | string | 模型名 |
| | `success` | 0/1 | 成败 |
| | `latency_ms` | number | 延迟 |
| | `error_message` | string? | 失败原因 |
| | `account_name` | string? | 账户名（账户删除后保留快照） |
| ActivityResponse | `entries` | ActivityEntry[] | 最近 N 条 |
| | `totalRequests` | number | **全表**总请求数 |
| | `successCount` | number | 全表成功数 |

### 3.6 健康

| 对象 | 字段 | 类型 | 含义 |
|------|------|------|------|
| HealthItem | `id` | string | `acc_{id}` |
| | `name` | string | 账户名 |
| | `status` | enum | `healthy` / `degraded` / `down` / `unknown`（按成功率：>0.9/>0.5/否则/无请求） |
| | `lastSuccess` | number | 成功数 |
| | `totalChecks` | number | 总请求数 |
| ModelHealthItem | `account_id` | number | |
| | `vendor_id` | string? | |
| | `model` | string | 模型名 |
| | `last_checked` | number | unix 秒 |
| | `success` | 0/1 | |
| | `latency` | number | ms |
| | `error` | string? | |
| | `limits_cache` | object? | Token 限额（`x-ratelimit-remaining-tokens` / `x-quota-remaining` 等） |
| | `limits_cache_updated_at` | string? | |
| | `account_name` | string? | |

### 3.7 可用模型（模型库）

| 字段 | 类型 | 含义 |
|------|------|------|
| `id` | string | 模型 id（上游 id 或 Gemini name 去 `models/` 前缀） |
| `name` | string | 显示名 |
| `object` | string? | 缺省 `"model"` |
| `created` | number? | 缺省 0 |
| `owned_by` | string | **网关插入**：提供该模型的厂商 id |
| `error` | string? | **仅占位对象**出现（账户模型拉取失败，`id="{account}-models-unavailable"`） |
| 其余字段 | — | 上游原样透传（openai 的 `owned_by`、gemini 的 `description` 等） |

响应外壳：`{ data: AvailableModel[], stale: boolean, cached_at: number }`

### 3.8 本机工具检测

`InstalledTools`：`{ vscode, claude, gemini, opencode, codex }` 全 boolean。

### 3.9 工具配置

| 工具 | GET 返回字段 |
|------|-------------|
| Claude | `{ exists, settings: object?, error? }` — settings 含 `env` 段：`ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_DEFAULT_OPUS_MODEL` / `_SONNET_` / `_HAIKU_` |
| Codex | `{ exists, auth: object?, configToml: string? }` — auth 含 `OPENAI_API_KEY` |
| Gemini | `{ exists, env: string?, settings: string? }` — **env 与 settings 都是原文 string**（非对象） |

写入响应：`{ success, backupPath, settings }`（settings 为写入后的配置结构）。

### 3.10 配置备份

| 对象 | 字段 | 类型 | 含义 |
|------|------|------|------|
| Backup 列表项 | `name` | string | 备份名（如 `settings.json.2026-08-06-120000`） |
| | `path` | string | 绝对路径 |
| | `timestamp` | string | `YYYY-MM-DD HH:MM:SS` |
| | `size` | number | 字节 |
| 备份内容 | `settings` | object | 读取/还原时返回 |

### 3.11 系统设置（KV）

`GET /api/settings` → 平铺键值对象：`{ "gateway_key": "...", "theme": "dark", ... }`
（key 任意，value 为 JSON 解析结果。）

### 3.12 配置导出 / 导入

| 对象 | 字段 | 含义 |
|------|------|------|
| ConfigExport | `version` | 当前 2 |
| | `accounts[]` | `{ id, vendor_id, name, api_key(明文), base_url?, anthropic_base_url?, openai_compatible, enabled, weight, notes? }` |
| | `aliases[]` | `{ alias, target_model, vendor_id?, account_ids[], preferred_account_id? }` |
| | `keys[]` | `{ name, key(明文), allowed_models[] }` |
| | `settings[]` | `{ key, value }`（不含 gateway_key） |
| 导入结果 | `{ success, imported: { accounts, aliases, keys } }` | 各计数 |

### 3.13 模型测试（规划 API 的输入输出）

| 对象 | 字段 | 含义 |
|------|------|------|
| 单测结果 | `{ success, latency, status, response, error }` | 失败也 200，用 success 区分；`response` 为上游响应体 |
| 批量队列 | 启动 `{ success, message, total }`；进度 `{ isRunning, total, current, progress(0-100) }` | |

### 3.14 用量统计（后端待暴露路由，数据形态预期）

| 数据 | 形态 |
|------|------|
| summary | `{ totalRequests, successRequests, ... }` |
| breakdown | 按 vendor / model / account 各维度的计数分布 |
| failover | 回退触发次数、按账户分布、成功率提升 |

---

## 4. 实体关系（数据层面，非 UI 布局）

```
Vendor 厂商 ─1:N─ Account 账户（账户挂厂商下，URL 可覆盖厂商默认）
ModelAlias 别名 ─N:M─ Account 账户（绑定表，最多一个首选，is_preferred）
ApiKey 密钥 ─1:N─ 白名单模型名（空 = 不限制）
Account 账户 ─1:N─ 活动记录（账户删除后 activity 保留 name 快照）
Vendor 删除被账户引用 → 拒绝（409）
别名与密钥白名单互不影响（同名也各自独立）
```

---

## 5. 业务规则与状态语义（UI 交互设计的约束）

### 5.1 状态枚举
- 账户/密钥启停：`0/1`
- 健康：`healthy` > `degraded` > `down` > `unknown`（按成功率阈值 0.9 / 0.5）
- 厂商协议：`openai` / `anthropic` / `gemini` / `custom`
- 测试结果：成功 = `success: true` + `latency`；失败 = `success: false` + `error`（仍 200）
- 白名单：`"*"`（全部）/ 模型数组 / 空（异常态）

### 5.2 关键业务约束
- **内置厂商不可删除**；自定义厂商被账户引用时删除被拒（409）
- **网关密钥明文只在创建时返回一次**；厂商 API Key 明文不可回读（只能重填）
- 删除账户 → 级联解除别名绑定，历史用量保留快照
- 删除别名 → **不影响**同名密钥白名单
- 编辑账户 API Key 传 `"********"`/空 = 不改密文；改 base_url 会触发重新校验
- 编辑厂商 URL 显式传 `null` = 清空（合并更新，缺省字段保留）
- 密钥白名单编辑传 `null` = 不更新（避免误清空 → 受限 key 变不限）
- 清空数据库：保留厂商目录 + 网关密钥，清账户/别名/密钥/用量
- WebSession 未知 provider → 自动创建以 provider 为 id 的独立厂商（不混入 openai 池）

### 5.3 数据敏感层级（UI 应区分对待）
| 级别 | 数据 | 交互要求 |
|------|------|---------|
| 高 | 厂商 API Key（不可回读）、网关密钥明文（一次性）、配置导出（含明文 key） | 掩码、仅创建时展示、导出为下载附件 |
| 中 | 账户/别名/白名单 | 常规管理 |
| 低 | 活动、健康、用量 | 常规展示 |

### 5.4 错误响应统一格式
- 绝大多数非 200：`{ "error": string }`（UI 直接展示 message 原文）
- 网关鉴权错误：`{ "error": { message, type, code } }`
- 例外：单测无活跃账户 → **200** `{ success: false, error }`；队列已在跑 → **409** `{ error }`

---

## 6. 数据来源（API 对照）

| 数据模型 | API |
|---------|-----|
| 账户 | `GET/POST /api/accounts`、`PUT/DELETE /api/accounts/{id}`、`GET /api/accounts/{id}/export` |
| 厂商 | `GET/POST /api/vendors`、`PUT/DELETE /api/vendors/{id}` |
| 别名 | `GET/POST /api/models/aliases`、`DELETE /api/models/aliases/{id}` |
| 可用模型 | `GET /api/models/available?force=` |
| 模型健康/测试 | `GET /api/models/health`、`POST /api/models/test`、`POST /api/models/test-all`、`GET /api/models/test-queue/status` |
| 密钥 | `GET/POST /api/keys`、`PUT/DELETE /api/keys/{id}` |
| 活动/健康 | `GET /api/activity?limit=`、`GET /api/health` |
| 工具配置/备份 | `GET/POST /api/system/{tools,claude,codex,gemini}-settings`、`-backups` |
| 设置/导入导出/清空 | `GET/PUT /api/settings`、`GET /api/export`、`POST /api/import`、`POST /api/settings/reset` |
| WebSession | `POST /api/auth/web-session` |
| 用量统计（待暴露） | `usage/summary`、`usage/breakdown`、`usage/failover` |

---

## 7. 设计师自由发挥的范围（明确边界）

**自由**：信息架构（页面/导航形态）、布局、组件样式、视觉风格、交互动效、数据可视化形态、明暗主题表现。

**不可变**：
1. 字段名 / 类型 / 枚举值（见 §3）——直接绑定后端契约
2. 业务约束（§5.2）——删除保护、一次性明文、白名单语义等
3. 数据敏感层级（§5.3）——掩码、一次性展示、下载附件等安全交互
