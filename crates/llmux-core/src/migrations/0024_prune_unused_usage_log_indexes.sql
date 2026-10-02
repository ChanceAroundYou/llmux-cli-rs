-- 删掉 5 个从未被任何查询用上的索引，再把第 6 个（被新索引顶替）也一并删掉。
--
-- 起因（2026-10-02 实测）：`usage_logs` 有 10 个索引，索引合计 14.0 MiB，
-- 而表本身只有 6.2 MiB —— 索引是表的 2.3 倍，且**每写一行都要更新全部 10 棵
-- B 树**（实测 5 万行/小时）。逐条跑 `EXPLAIN QUERY PLAN` 核对全部真实查询后，
-- 其中 5 个从未出现在任何一条计划里，第 6 个会被下面新建的索引顶掉。
--
-- 删掉的（`IF EXISTS` 兜住重复执行）：
--   idx_usage_logs_model             2.00 MiB  「按 model 聚合」实际走 is_test + 临时 B 树
--   idx_usage_logs_timestamp_model   2.48 MiB  无人使用
--   idx_usage_logs_timestamp_provider 1.49 MiB  「按 provider 聚合」走的是 provider_id 单列
--   idx_usage_logs_timestamp         1.03 MiB  被 idx_usage_logs_is_test 顶掉了
--   idx_usage_logs_timestamp_success 1.09 MiB  无人使用
--   idx_usage_logs_is_test                     ← 被下面新建的索引顶替
--
-- 补的：`(is_test, timestamp DESC)` 顶替 `idx_usage_logs_is_test`。
--   `/api/dashboard` 与 `/api/stats/logs` 都是
--   `WHERE is_test = 0 ORDER BY timestamp DESC LIMIT 100`，此前计划是
--     SEARCH l USING INDEX idx_usage_logs_is_test (is_test=?)
--     USE TEMP B-TREE FOR ORDER BY     ← 排序走临时 B 树
--   实测 79.87ms → 0.15ms（生产库副本，10.8 万行），且临时 B 树消失。
--   每次打开首页都付这个查询，所以这是本轮最大的一笔。
--
--   真正起作用的只有一件事：**timestamp 得在索引里**。单列 is_test 定位得到行，
--   却排不了序，排序被甩给临时 B 树，那 80ms 基本全花在这。
--   实测 `(timestamp DESC, is_test)` 与 `(is_test, timestamp)` 一样快（0.13ms，
--   同样没有临时 B 树 —— SQLite 干脆放弃 is_test 这个等值条件，直接按索引序往前扫），
--   并没有变成 COVERING。所以列顺序和 DESC 都不是关键，这里只是照着查询抄的写法。
--
--   留下的 4 个都有真实计划在用，不要动：
--   idx_usage_logs_account_id        health.rs 的「窗口内零流量 → 回退全历史」
--   idx_usage_logs_account_model     probe.rs 最近流量 / models/health
--   idx_usage_logs_account_timestamp health.rs 的 30 天窗口
--   idx_usage_logs_provider_id       按 provider 聚合（COVERING）
--   本文件新建的 idx_usage_logs_is_test_timestamp
--
-- ⚠️ 注意 `init_db` **没有迁移记录表**：23 个迁移每次启动全部重跑，靠吞掉
--   「already exists」错误来假装幂等。所以上面这 6 个索引会在每次启动时被
--   0001/0006/0013 重新建出来，再被本文件删掉 —— 实测每次启动白花 0.33s
--   建 6 棵 B 树（10.8 万行）。终态正确，代价可接受，不值得为它重构迁移机制。
--   **别去改 0001/0006/0013**：改了就和已发布的历史迁移对不上，比 0.33s 更麻烦。

DROP INDEX IF EXISTS idx_usage_logs_model;
DROP INDEX IF EXISTS idx_usage_logs_timestamp_model;
DROP INDEX IF EXISTS idx_usage_logs_timestamp_provider;
DROP INDEX IF EXISTS idx_usage_logs_timestamp;
DROP INDEX IF EXISTS idx_usage_logs_timestamp_success;

DROP INDEX IF EXISTS idx_usage_logs_is_test;
CREATE INDEX IF NOT EXISTS idx_usage_logs_is_test_timestamp
    ON usage_logs(is_test, timestamp DESC);
