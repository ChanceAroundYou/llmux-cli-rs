-- endpoint unification: chat_endpoint is written but no longer read;
-- base_url is the single read source for chat. Backfill legacy rows whose
-- endpoint lives in chat_endpoint only.
UPDATE accounts SET base_url = chat_endpoint
WHERE (base_url IS NULL OR base_url = '') AND chat_endpoint IS NOT NULL AND chat_endpoint != '';
