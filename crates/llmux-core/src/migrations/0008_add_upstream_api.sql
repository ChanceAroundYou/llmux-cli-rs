ALTER TABLE model_aliases ADD COLUMN upstream_api TEXT DEFAULT 'chat';
ALTER TABLE aggregate_aliases ADD COLUMN upstream_api TEXT DEFAULT 'chat';
