ALTER TABLE accounts ADD COLUMN chat_endpoint TEXT;
ALTER TABLE accounts ADD COLUMN responses_endpoint TEXT;
ALTER TABLE accounts ADD COLUMN messages_endpoint TEXT;
ALTER TABLE accounts ADD COLUMN default_protocol TEXT DEFAULT 'chat';
UPDATE accounts SET
  chat_endpoint = CASE WHEN provider_id IN ('openai','gemini','custom') THEN base_url END,
  messages_endpoint = CASE WHEN provider_id IN ('anthropic','custom-anthropic') THEN COALESCE(anthropic_base_url, base_url)
                           WHEN provider_id='custom' AND anthropic_base_url IS NOT NULL THEN anthropic_base_url END,
  responses_endpoint = NULL,
  default_protocol = CASE WHEN chat_endpoint IS NOT NULL OR base_url IS NOT NULL THEN 'chat'
                          WHEN messages_endpoint IS NOT NULL THEN 'messages' ELSE 'chat' END
WHERE chat_endpoint IS NULL AND messages_endpoint IS NULL AND responses_endpoint IS NULL;
-- fixup where CASE left chat_endpoint NULL but base_url existed (custom without provider match):
UPDATE accounts SET chat_endpoint = base_url WHERE chat_endpoint IS NULL AND base_url IS NOT NULL AND provider_id='custom' AND messages_endpoint IS NULL;
UPDATE accounts SET default_protocol = CASE WHEN chat_endpoint IS NOT NULL THEN 'chat' WHEN messages_endpoint IS NOT NULL THEN 'messages' ELSE 'chat' END WHERE default_protocol IS NULL OR default_protocol='';
UPDATE model_aliases SET upstream_api='default' WHERE upstream_api='auto';
UPDATE aggregate_aliases SET upstream_api='default' WHERE upstream_api='auto';
