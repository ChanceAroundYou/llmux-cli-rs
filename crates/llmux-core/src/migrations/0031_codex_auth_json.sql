-- Codex 订阅上游的原生化凭据存储。
-- 存**整个** auth.json（不只是三要素）：sidecar 的写回路径要保留
-- `auth_mode`/`last_refresh`/`account_id` 等所有字段，只存三要素会在
-- refresh 写回时把它们丢掉。空 = 未配置，回退到现有 sidecar 直连路径。
ALTER TABLE accounts ADD COLUMN codex_auth_json TEXT;
