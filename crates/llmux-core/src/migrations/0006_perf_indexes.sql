-- R5: missing indexes for stats/breakdown hot queries
CREATE INDEX IF NOT EXISTS idx_usage_logs_provider_id ON usage_logs(provider_id);
CREATE INDEX IF NOT EXISTS idx_usage_logs_model ON usage_logs(model);
CREATE INDEX IF NOT EXISTS idx_usage_logs_is_test ON usage_logs(is_test);
CREATE INDEX IF NOT EXISTS idx_usage_logs_timestamp_provider ON usage_logs(timestamp, provider_id);
CREATE INDEX IF NOT EXISTS idx_usage_logs_timestamp_model ON usage_logs(timestamp, model);
CREATE INDEX IF NOT EXISTS idx_api_keys_key ON api_keys(key);
CREATE INDEX IF NOT EXISTS idx_model_aliases_alias ON model_aliases(alias);
