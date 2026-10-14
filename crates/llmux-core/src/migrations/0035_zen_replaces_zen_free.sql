-- zen_free 退役，zen 原生直连。2026-10-14 实测结论：免费档的门是两个纯
-- HTTP 头（UA + ses_），llmux 自己的 reqwest+rustls 栈带上就 200 ——
-- 0034 的前提（「TLS 指纹拦截，只能旁挂 Node」）是误判，那次 curl 实验
-- 混杂了手搓的假 ses_ 值。sidecar 下线，llmux 原生打 `opencode.ai/zen/v1`。
--
-- 1. 类别改名：zen_free → zen。应用范围由 category 唯一决定（spec §2），
--    go 账户是 custom，不受影响。
-- 2. sidecar 前缀 → 直连地址：凡四个端点字段还指着已下线的 sidecar
--    （http://zen-free:6446/v1）的，一律改写成 https://opencode.ai/zen/v1。
--    非 sidecar 的自定义地址不动 —— 那是用户手配的（spec 选了"账户自配"）。
--
-- 无 WHERE 的 UPDATE 幂等，重复启动重跑无害（本仓迁移无记录表，见 AGENTS.md）。
UPDATE accounts SET category = 'zen' WHERE category = 'zen_free';

UPDATE accounts SET base_url = 'https://opencode.ai/zen/v1'
WHERE category = 'zen' AND base_url = 'http://zen-free:6446/v1';

UPDATE accounts SET chat_endpoint = 'https://opencode.ai/zen/v1'
WHERE category = 'zen' AND chat_endpoint = 'http://zen-free:6446/v1';

UPDATE accounts SET responses_endpoint = 'https://opencode.ai/zen/v1'
WHERE category = 'zen' AND responses_endpoint = 'http://zen-free:6446/v1';

UPDATE accounts SET messages_endpoint = 'https://opencode.ai/zen/v1'
WHERE category = 'zen' AND messages_endpoint = 'http://zen-free:6446/v1';
