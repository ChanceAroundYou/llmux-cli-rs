CREATE TABLE IF NOT EXISTS aggregate_aliases (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  alias         TEXT NOT NULL UNIQUE,
  candidates    TEXT NOT NULL,
  interval_secs INTEGER NOT NULL DEFAULT 300,
  created_at    DATETIME DEFAULT CURRENT_TIMESTAMP,
  updated_at    DATETIME DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_aggregate_aliases_alias ON aggregate_aliases(alias);
