-- zen_free 账户的上游地址同样不该由用户配置：`opencode.ai/zen/v1` 免费档按
-- TLS ClientHello 指纹拦截（curl / reqwest 一律 403 `FreeTierError`，只有 Node
-- 能过），llmux 只能经旁挂的 `zen-free` sidecar 走 compose 内网。地址配成直连
-- 唯一的后果就是 403；配成别的则指向一个不存在的容器。
--
-- 写入侧（create/update）已把 zen_free 端点钉死，这里补存量行。
-- 无 WHERE 的 UPDATE 幂等，重复启动重跑无害。
--
-- 三列填**同一个**前缀：sidecar 实现了 /v1/chat/completions、/v1/responses、
-- /v1/messages 三种形状（见 deploy/zen-free/server.mjs 的路由表），哪种能通是
-- per-model 的事 —— 由 `probeable` 探测决定，不是配置决定。
UPDATE accounts SET
    base_url = 'http://zen-free:6446/v1',
    chat_endpoint = 'http://zen-free:6446/v1',
    responses_endpoint = 'http://zen-free:6446/v1',
    messages_endpoint = 'http://zen-free:6446/v1',
    anthropic_base_url = NULL
WHERE category = 'zen_free';
