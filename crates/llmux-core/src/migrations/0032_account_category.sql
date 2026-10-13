-- 账户类别：决定后端路由/处理路径，与 provider_id（用户自定义标识）并存。
-- 封闭枚举，存字符串：
--   custom    默认。普通 OpenAI/Anthropic 兼容上游，无特殊处理。
--   codex     ChatGPT Plus 订阅原生路径（codex_compat.rs：私有后端 + 私有头
--             + payload 预处理 + 按需刷新凭据）。
--   zen_free  OpenCode Zen free tier。先占位，暂无特殊路由逻辑；上游走
--             zen-free sidecar 时表现为普通 custom 账户。
-- 迁移：provider_id='codex' 的账户归 codex（历史唯一的路由判据就是它），
-- 其余归 custom。空/未知值统一读写为 custom。
ALTER TABLE accounts ADD COLUMN category TEXT NOT NULL DEFAULT 'custom';

UPDATE accounts SET category = 'codex' WHERE provider_id = 'codex';
