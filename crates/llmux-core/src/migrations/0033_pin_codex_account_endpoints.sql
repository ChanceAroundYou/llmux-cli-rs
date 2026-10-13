-- codex 账户的上游地址是代码固定的（`codex_compat::CODEX_BACKEND_BASE`），
-- 不接受用户配置。存量行里这些字段还是已退役 sidecar 的地址
-- `http://codex-api:18080/v1`（容器 2026-10-13 已从 compose 删除，
-- `docker ps -a` 里连停止态都没有），页面照旧显示，排障时会照着它去找一个
-- 不存在的容器。
--
-- 写入侧（create/update）已经把 codex 端点钉死，这里补存量行。
-- 无 WHERE 的 UPDATE 是幂等的，重复启动重跑无害。
--
-- chat/messages 清空：codex 后端只有 responses 一种形状（`…/backend-api/codex`
-- 拼不出 `/chat/completions`），留着 chat 端点只会让别名配成 /chat 时静默
-- 拼出一个不存在的路径。
UPDATE accounts SET
    base_url = 'https://chatgpt.com/backend-api/codex',
    responses_endpoint = 'https://chatgpt.com/backend-api/codex',
    chat_endpoint = NULL,
    messages_endpoint = NULL,
    anthropic_base_url = NULL,
    default_protocol = 'responses'
WHERE category = 'codex';

-- 别名沿用旧的 /chat 会让每一轮拨测都报一次「配置可能写错了」。
-- 运行时（protocol::target_protocol）已经兜底强制 responses，这里把库里那个
-- 永远不该生效的值一并改掉 —— 状态要与行为一致，否则下次还得靠人记着。
UPDATE model_aliases SET upstream_api = 'responses'
WHERE upstream_api = 'chat'
  AND EXISTS (
    SELECT 1 FROM accounts
    WHERE accounts.category = 'codex'
      AND (
        (model_aliases.account_ids IS NOT NULL
         AND EXISTS (SELECT 1 FROM json_each(model_aliases.account_ids)
                     WHERE json_each.value = accounts.id))
        OR (model_aliases.account_ids IS NULL
            AND model_aliases.provider_id = accounts.provider_id)
      )
  );

UPDATE aggregate_aliases SET upstream_api = 'responses'
WHERE upstream_api = 'chat'
  AND EXISTS (
    SELECT 1 FROM json_each(aggregate_aliases.candidates) c
    JOIN accounts ON accounts.id = json_extract(c.value, '$.account_id')
    WHERE accounts.category = 'codex'
  );
