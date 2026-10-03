-- usage_logs 记录「这条请求是用哪把网关密钥进来的」。
--
-- 起因：统计此前只有模型 / 账号 / 厂商三个维度，无法按密钥切分。而这不是
-- 「加个 WHERE」能解决的 —— 这一列此前**根本没记**，历史行补不回来
-- （旧行 api_key_id 为 NULL，筛选部署前的时间窗必然为空，这是预期行为）。
--
-- 不加索引：现有 idx_usage_logs_is_test_timestamp (is_test, timestamp DESC)
-- 已能按窗口收窄，api_key_id 只是在其结果上再过一层。本仓库有明确规矩：
-- 索引是有限的资源，0024 刚清掉 6 棵从未出现在任何执行计划里的 B 树。
-- 补索引前先 EXPLAIN QUERY PLAN 看它会不会被选中，改完记得 ANALYZE 再对比。

ALTER TABLE usage_logs ADD COLUMN api_key_id INTEGER;