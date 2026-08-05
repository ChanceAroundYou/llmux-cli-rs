-- LLMux 全新 schema（2026-08-05 spec 推倒重来版）
-- 配置域 / 权限域 / 监控域 / 运行时域 严格分离，关系全部用外键表达。

-- 配置域：厂商目录（内置种子 + 用户自建）
CREATE TABLE IF NOT EXISTS vendors (
  id                    TEXT PRIMARY KEY,
  name                  TEXT NOT NULL,
  protocol              TEXT NOT NULL CHECK(protocol IN ('openai','anthropic','gemini','custom')),
  default_base_url      TEXT,
  default_anthropic_url TEXT,
  builtin               INTEGER NOT NULL DEFAULT 0,
  created_at            TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- 配置域：上游账户（凭据）
CREATE TABLE IF NOT EXISTS accounts (
  id                     INTEGER PRIMARY KEY AUTOINCREMENT,
  vendor_id              TEXT NOT NULL REFERENCES vendors(id),
  name                   TEXT NOT NULL UNIQUE,
  api_key_enc            TEXT NOT NULL,
  base_url               TEXT,
  anthropic_base_url     TEXT,
  enabled                INTEGER NOT NULL DEFAULT 1,
  weight                 INTEGER NOT NULL DEFAULT 1,
  notes                  TEXT,
  limits_cache           TEXT,
  limits_cache_updated_at TEXT,
  created_at             TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- 配置域：路由规则
CREATE TABLE IF NOT EXISTS model_aliases (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  alias         TEXT NOT NULL UNIQUE,
  target_model  TEXT NOT NULL,
  vendor_id     TEXT REFERENCES vendors(id),
  created_at    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- 配置域：alias↔账户 绑定（替代 account_ids JSON）
CREATE TABLE IF NOT EXISTS model_alias_accounts (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  alias_id     INTEGER NOT NULL REFERENCES model_aliases(id) ON DELETE CASCADE,
  account_id   INTEGER NOT NULL REFERENCES accounts(id)    ON DELETE CASCADE,
  position     INTEGER NOT NULL DEFAULT 0,
  is_preferred INTEGER NOT NULL DEFAULT 0,
  UNIQUE (alias_id, account_id)
);
-- 每个 alias 最多一个首选账户
CREATE UNIQUE INDEX IF NOT EXISTS uq_alias_one_preferred
  ON model_alias_accounts(alias_id) WHERE is_preferred = 1;

-- 权限域：网关鉴权
-- 网关 key = 各厂商账户 key 的集合，厂商 key 已加密（accounts.api_key_enc）；
-- 网关自己的 key 无需加密，明文存储（单用户本地网关），创建后可回读并一键写入工具配置。
CREATE TABLE IF NOT EXISTS api_keys (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  name         TEXT NOT NULL,
  key          TEXT NOT NULL UNIQUE,
  enabled      INTEGER NOT NULL DEFAULT 1,
  last_used_at TEXT,
  created_at   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- 权限域：key 的模型白名单（空表 = 不限制）
CREATE TABLE IF NOT EXISTS api_key_models (
  api_key_id  INTEGER NOT NULL REFERENCES api_keys(id) ON DELETE CASCADE,
  model       TEXT NOT NULL,
  PRIMARY KEY (api_key_id, model)
);

-- 监控域：最近活动 / 成功率数据源（无 token 列）
CREATE TABLE IF NOT EXISTS usage_logs (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  ts           INTEGER NOT NULL,
  account_id   INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
  account_name TEXT,
  model        TEXT,
  latency_ms   INTEGER DEFAULT 0,
  success      INTEGER NOT NULL DEFAULT 0,
  error_message TEXT
);
CREATE INDEX IF NOT EXISTS idx_usage_ts       ON usage_logs(ts);
CREATE INDEX IF NOT EXISTS idx_usage_account  ON usage_logs(account_id);
CREATE INDEX IF NOT EXISTS idx_usage_model    ON usage_logs(model);
CREATE INDEX IF NOT EXISTS idx_usage_success  ON usage_logs(success);

-- 运行时域：回退粘滞路由持久化
CREATE TABLE IF NOT EXISTS dispatch_state (
  dispatch_key          TEXT PRIMARY KEY,
  mode                  TEXT NOT NULL CHECK(mode IN ('primary','fallback')),
  sticky_fallback_id    INTEGER,
  consecutive_successes INTEGER DEFAULT 0,
  last_probe_ms         INTEGER DEFAULT 0,
  probe_backoff_secs    INTEGER DEFAULT 0,
  updated_at            TEXT DEFAULT CURRENT_TIMESTAMP
);

-- 配置域：类型化 key-value
CREATE TABLE IF NOT EXISTS app_settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- 种子数据：内置厂商目录
INSERT OR IGNORE INTO vendors (id, name, protocol, default_base_url, default_anthropic_url, builtin) VALUES
('openai',     'OpenAI',             'openai',    'https://api.openai.com/v1',                              NULL, 1),
('anthropic',  'Anthropic',          'anthropic', 'https://api.anthropic.com/v1',                           NULL, 1),
('gemini',     'Google Gemini',      'gemini',    'https://generativelanguage.googleapis.com/v1beta',       NULL, 1),
('deepseek',   'DeepSeek',           'openai',    'https://api.deepseek.com',                            NULL, 1),
('moonshot',   'Moonshot AI',        'openai',    'https://api.moonshot.cn/v1',                             NULL, 1),
('zhipu',      '智谱 GLM',           'openai',    'https://open.bigmodel.cn/api/paas/v4',                   NULL, 1),
('siliconflow', 'SiliconFlow',       'openai',    'https://api.siliconflow.cn/v1',                          NULL, 1),
('zai',        '阶跃星辰 StepFun',   'openai',    'https://api.stepfun.com/v1',                             NULL, 1),
('huoshan',    '火山方舟 Ark',       'openai',    'https://ark.cn-beijing.volces.com/api/v3',               NULL, 1);
