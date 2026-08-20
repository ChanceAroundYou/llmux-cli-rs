CREATE TABLE IF NOT EXISTS account_model_cache (
  account_id INTEGER PRIMARY KEY,
  alias TEXT NOT NULL,
  models_json TEXT NOT NULL DEFAULT '[]',
  error TEXT,
  updated_at INTEGER NOT NULL
);
