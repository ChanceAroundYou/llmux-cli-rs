-- 请求归属：客户端请求的模型/别名、实际路由的聚合别名、命中的候选下标。
-- 旧行为 NULL，不回填（无法可靠还原历史请求的路由）。

ALTER TABLE usage_logs ADD COLUMN requested_model TEXT;
ALTER TABLE usage_logs ADD COLUMN aggregate_alias TEXT;
ALTER TABLE usage_logs ADD COLUMN candidate_idx INTEGER;

-- 聚合别名筛选：等值 alias + 时间范围。其余维度暂用现有时间索引收窄。
CREATE INDEX IF NOT EXISTS idx_usage_logs_aggregate_alias_ts
  ON usage_logs(aggregate_alias, timestamp);
