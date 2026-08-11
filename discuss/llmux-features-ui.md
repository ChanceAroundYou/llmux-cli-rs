# LLMux 功能点清单（UI 开发参考）

> 对应全部已登记的 API（Swagger `/swagger`，33 paths）。`[现状]` 标注：✅已有 / 🟡部分 / ❌未做（后端有接口、UI 未挂）。

## 1. 仪表盘 Dashboard

| 功能点 | 说明 | API | 现状 |
|--------|------|-----|------|
| 活动流 | 最近请求列表（模型/账户/成功/延迟/错误） | `GET /api/activity` | ✅ |
| 用量统计 | 总请求数 / 成功数（全表统计） | `GET /api/activity` | ✅ |
| 健康状态 | 网关健康、账户/上游在线情况 | `GET /api/health` | ✅ |
| 账户概览 | 账户数量/启停 | `GET /api/accounts` | ✅ |

## 2. 账户管理 Accounts

| 功能点 | 说明 | API | 现状 |
|--------|------|-----|------|
| 账户列表 | 全部账户 + 厂商 + 状态 | `GET /api/accounts` | ✅ |
| 创建账户 | vendor_id/name/api_key + 自定义 base_url | `POST /api/accounts` | ✅ |
| 更新账户 | 名称/启停/权重/备注/base_url | `PUT /api/accounts/{id}` | ✅ |
| 删除账户 | 删除（级联清绑定） | `DELETE /api/accounts/{id}` | ✅ |
| 账户自定义 URL | 账户级 base_url/anthropic_base_url 覆盖厂商默认 | 创建/更新时填 | ✅ |
| 连通性校验 | 创建/更新时自动拉上游模型列表验证 | 创建/更新响应 | ✅ |
| 用量 CSV 导出 | 单个账户的历史用量 CSV | `GET /api/accounts/{id}/export` | ❌（后端有） |

## 3. 厂商管理 Vendors

| 功能点 | 说明 | API | 现状 |
|--------|------|-----|------|
| 厂商列表 | 内置种子 + 自定义厂商，含协议/URL/coding | `GET /api/vendors` | ✅（展示） |
| 创建厂商 | 自定义厂商（id/name/协议/protocols/URL） | `POST /api/vendors` | 🟡 API 层 |
| 更新厂商 | 合并更新（含 coding_plan 开关） | `PUT /api/vendors/{id}` | 🟡 API 层 |
| 删除厂商 | 删除（被账户引用时 409） | `DELETE /api/vendors/{id}` | 🟡 API 层 |
| 厂商管理 UI | 独立的厂商管理页面 | — | ❌（用户定暂不做） |

> 厂商字段：protocol（主协议）、protocols（多协议数组）、openai_responses（Responses 支持）、default_base_url、default_anthropic_url、coding_plan + coding_base_url + coding_anthropic_url

## 4. 网关 Key 管理 Keys

| 功能点 | 说明 | API | 现状 |
|--------|------|-----|------|
| key 列表 | 网关 key + 白名单 | `GET /api/keys` | ✅ |
| 创建 key | 生成 sk-llmux-*（明文回读） | `POST /api/keys` | ✅ |
| 更新 key | 改名 / 白名单（null 不清空） | `PUT /api/keys/{id}` | ✅ |
| 删除 key | 删除 | `DELETE /api/keys/{id}` | ✅ |
| 模型白名单 | 限制 key 可访问的模型（空=不限制） | 创建/更新时填 | ✅ |
| 一键复制 | 复制 key 到剪贴板 | — | ✅ |

## 5. 模型 / 别名管理 Models

| 功能点 | 说明 | API | 现状 |
|--------|------|-----|------|
| 别名列表 | 别名 + 绑定账户集（含厂商/协议/preferred） | `GET /api/models/aliases` | ✅ |
| 创建别名 | alias → target_model + 绑定账户 | `POST /api/models/aliases` | ✅ |
| 删除别名 | 删除（不清同名白名单） | `DELETE /api/models/aliases/{id}` | ✅ |
| 可用模型 | 从上游拉取模型目录（缓存 24h） | `GET /api/models/available` | ✅ |
| 模型健康 | 各模型/账户健康状态 | `GET /api/models/health` | ✅ |
| 单模型测试 | 测试一个模型连通性 | `POST /api/models/test` | ✅ |
| 批量测试 | 启动全量测试队列 | `POST /api/models/test-all` | ✅ |
| 测试进度 | 队列进度（isRunning/total/current/progress） | `GET /api/models/test-queue/status` | ✅ |

## 6. 用量监控 Usage

| 功能点 | 说明 | API | 现状 |
|--------|------|-----|------|
| 最近活动 | 活动列表 | `GET /api/activity` | ✅ |
| 全表统计 | totalRequests/successCount | `GET /api/activity` | ✅ |

> 后端另有 `get_summary` / `get_breakdown_by_vendor/model/account` / `get_failover_stats`（service 层，未暴露路由，UI 如需可加）

## 7. 设置 Settings

| 功能点 | 说明 | API | 现状 |
|--------|------|-----|------|
| 读取设置 | 键值对 | `GET /api/settings` | ✅ |
| 更新设置 | 批量写入 | `PUT /api/settings` | ✅ |
| 导出配置 | 全量配置 JSON（账户/别名/key/设置，不含 gateway_key） | `GET /api/export` | ✅ |
| 导入配置 | 导入（非法返回 400） | `POST /api/import` | ✅ |
| 清空数据 | 重置运行数据（保留厂商/gateway key） | `POST /api/settings/reset` | ✅ |

## 8. 工具一键配置 Setup / System

| 功能点 | 说明 | API | 现状 |
|--------|------|-----|------|
| 工具检测 | 本机装了哪些 CLI（claude/codex/gemini/opencode/vscode） | `GET /api/system/tools` | ✅ |
| Claude Code 配置 | 读写 settings.json（含备份） | `GET/POST /api/system/claude-settings` | ✅ |
| Codex 配置 | 读写 auth.json + config.toml（含备份） | `GET/POST /api/system/codex-settings` | ✅ |
| Gemini 配置 | 读写 .env + settings.json（含备份） | `GET/POST /api/system/gemini-settings` | ✅ |
| 备份管理 | 各工具配置备份的列出/读取/恢复/删除 | `GET/POST/DELETE /api/system/{tool}-backups` | 🟡 后端有 |
| WebSession 导入 | 生成浏览器脚本捕获 session token 回连 | `POST /api/auth/web-session` | ✅（WebLoginWizard） |

## 9. 网关透传（客户端直连，UI 一般不用）

| 端点 | 协议 | 说明 |
|------|------|------|
| `POST /v1/chat/completions` | OpenAI | Chat Completions |
| `POST /v1/responses` | OpenAI | Responses API |
| `POST /v1/messages` | Anthropic | Messages API |
| `GET /v1/models` | 兼容 | 模型列表 |
| `POST /v1beta/{model}:{action}` | Gemini | Gemini 原生 |

## 10. 关于 About

| 功能点 | 说明 | 现状 |
|--------|------|------|
| 关于页 | 版本/信息 | ✅ |

---

## UI 现状缺口（后端已有接口、UI 未挂载）

1. **账户用量 CSV 导出**（`/api/accounts/{id}/export`）
2. **厂商管理 UI**（创建/编辑/删除厂商，含 coding_plan 开关）——用户之前定"暂不做"
3. **配置备份管理**（备份列表/恢复/删除）——setup 面板后端有接口

## 路由/协议的核心模型（UI 理解数据的关键）

- **厂商**（vendors）→ 默认 URL + 协议能力
- **账户**（accounts）→ 挂在厂商下，可自定义 URL 覆盖
- **别名**（aliases）→ 绑定账户集（跨厂商），请求 model=别名名 → 路由到绑定账户
- **网关 key**（api_keys）→ 客户端认证 + 模型白名单
- **三种协议**（OpenAI / Responses / Anthropic / Gemini）→ 同一批账户按协议路由
