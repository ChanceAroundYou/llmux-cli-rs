# LLMux 数据库全新设计（Spec）

> 日期：2026-08-05
> 性质：**推倒重来的全新 schema 设计**（不是对现有库的增量修补）
> 适用产品：本地单用户 AI 网关（账户、路由、网关鉴权、回退调度），存储引擎固定为 SQLite
> 目标库：直接建在 `~/.config/llmux-repair/`，**全新库，不复用/不迁移旧表**（见 §9）

---

## 1. 目标与原则

1. **一表一职责**：配置域、权限域、监控域、运行时域严格分离。
2. **关系全部用外键表达**：不出现 `[1,2]` 之类的 JSON 列；该加 `CHECK` / `UNIQUE` / 部分索引的地方就加。
3. **历史数据不因删配置而消失**：删账户保留监控记录（快照归因）。
4. **凭据安全**：厂商 api_key 加密存储（`accounts.api_key_enc`）；网关 key 明文存储（单用户本地网关有意设计，创建后可回读一键写入工具配置）。
5. **YAGNI**：页面用不到的表不进 schema。

---

## 2. 范围界定

### 做
- `vendors`（厂商目录）：内置种子 + 用户自建 + 持久复用，支持"选厂商自动填 base_url"
- `accounts` / `model_aliases` / `model_alias_accounts`（路由绑定）
- `api_keys` / `api_key_models`（网关鉴权 + 模型白名单）
- `usage_logs`（**最小化**：仅服务 dashboard 成功率/延迟/最近活动，无 token 列）
- `dispatch_state`（回退状态持久化，功能表）
- `app_settings`（类型化 key-value）

### 不做（YAGNI）
- ❌ `usage_daily` 日聚合表 —— UI 未用
- ❌ `failover_events` 回退事件表 —— 前端未调用其接口，调度逻辑本身不依赖它
- ❌ `health_checks` 健康历史表 —— 健康状态内存即可，重启重查
- ❌ usage_logs 的 token / cache token 列 —— 页面不显示 token
- ❌ `model_prices`（旧死表）→ 不再存在
- ❌ 旧的 `providers` 表 → 由 `vendors` 取代

---

## 3. 表结构

### 3.1 配置域

#### vendors —— 厂商目录

"选厂商自动填 base_url" 的数据源。内置一批常用厂商，用户也可自建，自建的会一直留在表里供后续复用。

```sql
CREATE TABLE vendors (
  id                    TEXT PRIMARY KEY,   -- 机器可读 id：'openai','anthropic','deepseek','moonshot','zai'…
  name                  TEXT NOT NULL,      -- 显示名：'OpenAI','DeepSeek'…
  protocol              TEXT NOT NULL CHECK(protocol IN ('openai','anthropic','gemini','custom')),
  default_base_url      TEXT,               -- 官方 OpenAI 兼容端点
  default_anthropic_url TEXT,               -- 可选：官方 Anthropic 兼容端点（没有则 NULL）
  builtin               INTEGER NOT NULL DEFAULT 0,   -- 1=内置种子 0=用户自建
  created_at            TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

#### accounts —— 上游账户（凭据）

```sql
CREATE TABLE accounts (
  id                     INTEGER PRIMARY KEY AUTOINCREMENT,
  vendor_id              TEXT NOT NULL REFERENCES vendors(id),
  name                   TEXT NOT NULL UNIQUE,   -- 展示名，同时是 import 去重键
  api_key_enc            TEXT NOT NULL,          -- AES-GCM 密文
  base_url               TEXT,                   -- NULL = 用 vendors.default_base_url
  anthropic_base_url     TEXT,                   -- NULL = 用 vendors.default_anthropic_url
  enabled                INTEGER NOT NULL DEFAULT 1,
  weight                 INTEGER NOT NULL DEFAULT 1,
  notes                  TEXT,
  limits_cache           TEXT,                   -- 供应商限额缓存（不透明字段）
  limits_cache_updated_at TEXT,
  created_at             TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

#### model_aliases —— 路由规则

```sql
CREATE TABLE model_aliases (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  alias         TEXT NOT NULL UNIQUE,   -- 请求里使用的模型名
  target_model  TEXT NOT NULL,          -- 发给上游的真实模型名
  vendor_id     TEXT REFERENCES vendors(id),   -- 无显式绑定时按此厂商路由
  created_at    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

#### model_alias_accounts —— alias↔账户 绑定（替代 `account_ids` JSON）

```sql
CREATE TABLE model_alias_accounts (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  alias_id     INTEGER NOT NULL REFERENCES model_aliases(id) ON DELETE CASCADE,
  account_id   INTEGER NOT NULL REFERENCES accounts(id)    ON DELETE CASCADE,
  position     INTEGER NOT NULL DEFAULT 0,   -- 勾选顺序
  is_preferred INTEGER NOT NULL DEFAULT 0,   -- 该集合内首选账户（1 表示）
  UNIQUE (alias_id, account_id)
);
-- 每个 alias 最多一个首选：部分唯一索引
CREATE UNIQUE INDEX uq_alias_one_preferred ON model_alias_accounts(alias_id) WHERE is_preferred = 1;
```

**路由语义**：alias 有绑定 → 只用绑定集（`is_preferred` 标记的账户排最前）；无绑定 → 按 `vendor_id` 路由该厂商下所有 `enabled` 账户（按 weight）。

### 3.2 权限域

#### api_keys —— 网关鉴权（明文存储，单用户本地网关）

> 实现修正（2026-08-06）：网关 key **明文存储**而非哈希。理由：单用户本地网关，
> 厂商 key 已单独加密（`accounts.api_key_enc`），网关 key 明文可回读、一键写入
> 工具配置（Claude Code / Codex / Gemini）。已知取舍：库文件泄露即网关 key 泄露。

```sql
CREATE TABLE api_keys (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  name         TEXT NOT NULL,
  key          TEXT NOT NULL UNIQUE,
  enabled      INTEGER NOT NULL DEFAULT 1,
  last_used_at TEXT,
  created_at   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

#### api_key_models —— key 的模型白名单（替代 allowed_models JSON）

```sql
CREATE TABLE api_key_models (
  api_key_id  INTEGER NOT NULL REFERENCES api_keys(id) ON DELETE CASCADE,
  model       TEXT NOT NULL,
  PRIMARY KEY (api_key_id, model)
);
-- 空表 = 不限制
```

### 3.3 监控域（最小化）

#### usage_logs —— 最近活动 / 成功率数据源（dashboard 依赖）

```sql
CREATE TABLE usage_logs (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  ts           INTEGER NOT NULL,          -- epoch 毫秒
  account_id   INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
  account_name TEXT,                      -- 写时快照，删账户后仍可归因
  model        TEXT,
  latency_ms   INTEGER DEFAULT 0,
  success      INTEGER NOT NULL DEFAULT 0,
  error_message TEXT
);
CREATE INDEX idx_usage_ts       ON usage_logs(ts);
CREATE INDEX idx_usage_account  ON usage_logs(account_id);
CREATE INDEX idx_usage_model    ON usage_logs(model);
CREATE INDEX idx_usage_success  ON usage_logs(success);
```

> dashboard 的"最近成功率 / 平均延迟 / 延迟脉冲图 / 最近请求列表"全部来自 `/api/activity` → 本表。**没有 token 列**（页面不显示，后续也不打算做成本统计）。

### 3.4 运行时域

#### dispatch_state —— 回退粘滞路由持久化

```sql
CREATE TABLE dispatch_state (
  dispatch_key          TEXT PRIMARY KEY,   -- provider:model 粒度
  mode                  TEXT NOT NULL CHECK(mode IN ('primary','fallback')),
  sticky_fallback_id    INTEGER,
  consecutive_successes INTEGER DEFAULT 0,
  last_probe_ms         INTEGER DEFAULT 0,
  probe_backoff_secs    INTEGER DEFAULT 0,
  updated_at            TEXT DEFAULT CURRENT_TIMESTAMP
);
```

### 3.5 配置域

#### app_settings —— 类型化配置

```sql
CREATE TABLE app_settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,   -- 一律 JSON 编码（字符串也带引号，避免类型篡改）
  updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);
```

---

## 4. 表间联动

### 4.1 外键关系

```
vendors ◄──────────────────── accounts.vendor_id                 (禁止删有账户引用的厂商)
accounts ◄─────────────────── model_alias_accounts.account_id    (ON DELETE CASCADE)
accounts ◄─────────────────── usage_logs.account_id              (ON DELETE SET NULL)
model_aliases ◄────────────── model_alias_accounts.alias_id      (ON DELETE CASCADE)
api_keys ◄─────────────────── api_key_models.api_key_id          (ON DELETE CASCADE)
```

### 4.2 删除语义

| 操作 | 联动结果 |
|---|---|
| 删厂商 | 仍有账户引用 → 被外键挡住，强制先处理账户 |
| 删账户 | ① 所有 alias 绑定自动解除（CASCADE）→ 绑定了死账户的 alias 自动变回"按厂商路由"；② 监控记录保留，`account_id` 置空但 `account_name` 快照仍在，dashboard 仍可归因；③ `dispatch_state` 里失效的 sticky 引用由调度器自然清理 |
| 删 alias | 绑定行自动清空（CASCADE） |
| 删 key | 白名单自动清空（CASCADE） |

### 4.3 运行时数据流

```
请求 model → resolve_model:
  model_aliases.alias = 请求名
    ├─ 有绑定 → JOIN model_alias_accounts → 精确账户集（is_preferred 优先）
    └─ 无绑定 → 按 vendor_id 路由（vendor 下 enabled 账户按 weight）
→ 选账户 → dispatch_state 读写（primary/fallback、探测退避）
→ 请求结束 → usage_logs 写一行（account_id + account_name 快照，无 token）
→ 健康检查 → 内存态（不入库）
```

---

## 5. 种子数据（内置厂商目录，可调整）

```sql
INSERT INTO vendors (id, name, protocol, default_base_url, default_anthropic_url, builtin) VALUES
('openai',    'OpenAI',             'openai',    'https://api.openai.com/v1',                              NULL, 1),
('anthropic', 'Anthropic',          'anthropic', 'https://api.anthropic.com/v1',                           NULL, 1),
('gemini',    'Google Gemini',      'gemini',    'https://generativelanguage.googleapis.com/v1beta',       NULL, 1),
('deepseek',  'DeepSeek',           'openai',    'https://api.deepseek.com/v1',                            NULL, 1),
('moonshot',  'Moonshot AI',        'openai',    'https://api.moonshot.cn/v1',                             NULL, 1),
('zhipu',     '智谱 GLM',           'openai',    'https://open.bigmodel.cn/api/paas/v4',                   NULL, 1),
('siliconflow','SiliconFlow',       'openai',    'https://api.siliconflow.cn/v1',                          NULL, 1),
('zai',       '阶跃星辰 StepFun',   'openai',    'https://api.stepfun.com/v1',                             NULL, 1),
('huoshan',   '火山方舟 Ark',       'openai',    'https://ark.cn-beijing.volces.com/api/v3',               NULL, 1);
```

用户自建厂商：`INSERT` 时 `builtin=0`，自己填 `protocol` + `default_base_url`。表持久化 → 下次加账户可直接复用。

---

## 6. 关键决策与理由

| 决策 | 选择 | 理由 |
|---|---|---|
| 存储引擎 | SQLite（不变） | 本地单用户，正确形态 |
| 厂商/协议 | 真实表 `vendors`，accounts 外键 | 收编散落的硬编码 URL；支持"选厂商自动填 base_url"；复用用户自建厂商 |
| `account_ids` | 规范化成 `model_alias_accounts` | 外键保证引用完整，JOIN 可查，删账户级联清理 |
| 首选账户 | `is_preferred` 标记 + 部分唯一索引 | 结构性保证"首选 ∈ 绑定集"，且每个 alias 最多一个首选 |
| api_key | 明文（单用户本地网关，可回读一键配置） | 库文件泄露会暴露网关密钥，属已知取舍 |
| usage_logs | 保留最小形态（无 token） | dashboard 成功率/延迟/活动依赖它；无 token 展示需求 |
| usage_daily / failover_events / health_checks | 不做 | UI 未使用，纯负担 |
| 删账户历史 | 保留 + `account_name` 快照 | 监控历史不因删配置丢失 |
| `dispatch_state` | 保留 | 功能表：重启不丢回退状态 |

---

## 7. 明确不做（YAGNI 清单）

- token / cache token 用量统计、成本计算
- 用量日/月聚合报表
- 健康检查历史持久化
- 多用户 / 团队 / 按 key 配额预算
- 回退事件审计

---

## 8. 本窗口未最终确认、按推荐值写入的点

以下两点在讨论中未逐条确认，文档按推荐值定稿；执行前如有异议可直接改：

1. **首选账户表达**：采用 `model_alias_accounts.is_preferred`（+ 每 alias 最多一个首选的唯一索引），而非 `model_aliases.preferred_account_id` 独立列。
2. **api_key 存储**：网关 key 明文存储（单用户本地网关有意设计，可回读一键配置）；厂商 key 加密存储于 `accounts.api_key_enc`。

---

## 9. 附：执行提示（开新窗口时）

- 代码已退回线上版本，此文档是新窗口的唯一蓝图。
- **端口与数据库路径由 `.env` 驱动**：线上 `PORT=25975` + 默认库（`~/.config/llmux`）；测试 `PORT=25999` + `DATA_DIR=~/.config/llmux-repair`（见 `.env.example` / `.env`）。代码内置默认端口 `25975`。
- 建议在新窗口先读本文档，再按域拆分实施：存储层 → dispatcher/路由 → API → UI（厂商选择器、绑定 UI）。
- **目标库直接建在 `~/.config/llmux-repair/`（全新库），不复用/不迁移旧表**：
  - `~/.config/llmux-repair/llmux_db.db` 里现有的旧 schema 表**全部弃用**，新实现直接按本文档 §3 建全新库（文件名可沿用 `llmux_db.db`，直接重建）。
  - **不做任何旧→新迁移**（不搬运 accounts/aliases/keys 旧数据）。
  - 旧数据（账户、别名、网关 key）在实现完成后**手动重新录入**（UI 或 API）。
  - `master.key` 已在该目录（0600），新库加密继续用它。
- 线上正式库 `~/.config/llmux/llmux_db.db` 保持不动，等新库验证通过后再决定切换方式。

---

## 10. 测试方案（防误操作）

> 目标：**任何测试操作都物理够不着线上库**。线上库被污染会中断正在使用该网关的会话（包括本会话），务必遵守以下规则。

### 10.1 两套环境对照

| | 线上（生产） | 测试 |
|---|---|---|
| 端口 | `25975`（当前旧实例 `25976`） | `25999` |
| 数据目录 | `~/.config/llmux` | `~/.config/llmux-repair` |
| 数据库 | `llmux_db.db` | `llmux_db.db`（全新 schema） |
| master.key | 线上原文件 | 复制件（与线上一致，0600） |
| 配置来源 | `.env`（线上值） | `.env`（测试值） |

`.env` 当前（测试值）：
```
PORT=25999
DATA_DIR=/Users/moody/.config/llmux-repair
```

### 10.2 安全规则（红黄绿）

- 🟢 **绿灯（安全）**：在仓库根目录跑 `cargo run`——根目录的 `.env` 指向测试库，物理隔离。
- 🟡 **黄灯（显式）**：任何目录跑都手动带 `DATA_DIR=~/.config/llmux-repair PORT=25999`。
- 🔴 **红灯（禁止）**：
  - 从**没有 `.env`** 的目录裸跑二进制（会落到线上默认 25975 + `~/.config/llmux`）
  - 把 `.env` 改成线上值后未确认就跑
  - 用 `git clean -fdx` 清仓库（会删掉 `.env` 和本 spec，删完再跑 = 红灯）

### 10.3 每次测试前后核对清单

**测试前**（10 秒）：
1. 确认线上实例在跑：`pgrep -fl llmux`（应有 PID 50278 之类）
2. 确认线上库完好：`sqlite3 ~/.config/llmux/llmux_db.db "PRAGMA integrity_check;"` → `ok`
3. 记录线上库文件大小：`ls -l ~/.config/llmux/llmux_db.db`

**测试中**（可疑时）：
- 确认新进程连的是修复库而不是线上库：
  ```bash
  lsof -p <PID> | grep llmux_db.db    # 必须出现 llmux-repair/llmux_db.db
  ```
- 确认端口是 25999：`curl -s http://localhost:25999/api/health`

**测试后**（收尾）：
1. 杀掉测试进程：`pkill -f "target/debug/llmux"`（不要误伤 50278）
2. 再次核对线上库未被改动：文件大小、`integrity_check`、journal 仍是 `delete`

### 10.4 切线上（主动、唯一允许碰线上库的时机）

1. 先备份：`sqlite3 ~/.config/llmux/llmux_db.db ".backup '~/.config/llmux/backup-<日期>.db'"`
2. 把 `.env` 改为线上值：
   ```
   PORT=25975
   # DATA_DIR=   ← 删掉或注释，用默认 ~/.config/llmux
   ```
3. 停旧实例 → 跑新二进制 → 验证健康与数据
4. 有问题随时回滚到备份

### 10.5 误操作的补救

若发现测试进程连上了线上库：
- **立即停掉该进程**，线上实例（50278）不受影响，继续提供服务
- SQLite 未提交的写不会落盘；已提交的写可用备份回滚
- 先做 `integrity_check`，再决定是否回滚
