CREATE TABLE IF NOT EXISTS accounts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  alias TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  api_key TEXT NOT NULL,
  base_url TEXT,
  anthropic_base_url TEXT,
  is_active INTEGER DEFAULT 1,
  weight INTEGER DEFAULT 1,
  notes TEXT,
  limits_cache TEXT,
  limits_cache_updated_at DATETIME,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS providers (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  type TEXT NOT NULL,
  base_url TEXT
);

CREATE TABLE IF NOT EXISTS model_aliases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  alias TEXT NOT NULL UNIQUE,
  target_model TEXT NOT NULL,
  provider_id TEXT,
  account_ids TEXT
);

CREATE TABLE IF NOT EXISTS model_prices (
  model_id TEXT PRIMARY KEY,
  vendor TEXT,
  input_price REAL,
  output_price REAL,
  updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS usage_logs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  timestamp INTEGER NOT NULL,
  account_id INTEGER,
  provider_id TEXT,
  model TEXT,
  input_tokens INTEGER DEFAULT 0,
  output_tokens INTEGER DEFAULT 0,
  cache_read_input_tokens INTEGER DEFAULT 0,
  cache_creation_input_tokens INTEGER DEFAULT 0,
  latency_ms INTEGER DEFAULT 0,
  success INTEGER DEFAULT 0,
  error_message TEXT,
  is_test INTEGER DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_usage_logs_timestamp ON usage_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_usage_logs_account_id ON usage_logs(account_id);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT
);

CREATE TABLE IF NOT EXISTS api_keys (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  key TEXT NOT NULL UNIQUE,
  allowed_models TEXT DEFAULT '*',
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

INSERT OR IGNORE INTO providers (id, name, type) VALUES ('openai', 'OpenAI', 'openai');
INSERT OR IGNORE INTO providers (id, name, type) VALUES ('anthropic', 'Anthropic', 'anthropic');
INSERT OR IGNORE INTO providers (id, name, type) VALUES ('gemini', 'Google Gemini', 'gemini');
INSERT OR IGNORE INTO providers (id, name, type) VALUES ('custom-anthropic', 'Custom (Anthropic Compatible)', 'custom-anthropic');
