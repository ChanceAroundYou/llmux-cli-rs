ALTER TABLE usage_logs ADD COLUMN ttft_ms INTEGER;
ALTER TABLE usage_logs ADD COLUMN is_stream INTEGER DEFAULT 0;
CREATE INDEX idx_usage_logs_timestamp_success ON usage_logs(timestamp, success);
