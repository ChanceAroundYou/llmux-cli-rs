-- 折算价从「按模型名」升级为「按上游账号 × 模型」。
--
-- 起因：同一个 deepseek-v4.1-flash 被 go2/go6/go7（OpenCode Go 订阅）、command、
-- DeepSeek 官方等多个上游服务，价各不相同 —— OpenRouter 的缓存读价（$0.03/M）
-- 比 OpenCode Go（$0.006/M）贵 5 倍。只按 model 取价会把它们算成同一个价。
--
-- 键是 (account_id, model_id)：account_id = usage_logs.account_id，model_id =
-- usage_logs.model（网关侧模型名）。读取侧按 usage_logs.account_id JOIN 本表。
--
-- 长上下文分档：Zen/Go 的 GPT/Claude 有 `≤272K` / `>272K` 两档，Qwen3.7 Plus 有
-- `≤256K` / `>256K`。threshold 非空时，prompt(input + cache_read) 超过它就用 long_* 价。
--
-- 峰谷（DeepSeek 系）按定下的口径**一律存峰价**：不存时段、不建节假日表，
-- 宁可高估（最多 2×，只在谷段）。
--
-- source: openrouter|zen|zen-go|deepseek|teamorouter|command|dashscope|manual|free。
-- 'manual' 行永不被自动刷新覆盖（沿用 0027 立下的规矩）。
--
-- 不加额外索引：主键 (account_id, model_id) 正好覆盖读取侧 JOIN；且
-- usage_logs(account_id) 已有 idx_usage_logs_account_id。
--
-- 旧的 model_prices 保留为「全局兜底目录」（OpenRouter 那批）：某个账号没有本表
-- 行时读取侧退回它，避免改造期间数字一夜归零。

CREATE TABLE IF NOT EXISTS upstream_prices (
  account_id   INTEGER NOT NULL,
  model_id     TEXT    NOT NULL,
  vendor       TEXT,
  input_price  REAL,
  output_price REAL,
  cache_read_price  REAL,
  cache_write_price REAL,
  long_context_threshold INTEGER,
  long_input_price  REAL,
  long_output_price REAL,
  long_cache_read_price  REAL,
  long_cache_write_price REAL,
  source       TEXT NOT NULL DEFAULT 'manual',
  source_model_id TEXT,
  updated_at   DATETIME DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (account_id, model_id)
);
