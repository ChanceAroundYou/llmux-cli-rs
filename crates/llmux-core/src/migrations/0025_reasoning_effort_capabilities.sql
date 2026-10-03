-- reasoning_effort 能力行：这个 deployment 实际接受哪些档位。
--
-- 定位是**缓存**，不是权限表：这张表记录观测，只用来收窄内置能力表
-- （`crates/llmux-core/src/reasoning_effort.rs` 的 static_capability）。
-- 没有它，进程每次重启都要把上游的脾气重新学一遍 —— 昨天拒了 max 的 provider
-- 今天还会再拒一次。
--
-- 为什么不加额外索引：唯一的查询是启动时全表
-- `SELECT provider, model, supported, rejected FROM reasoning_effort_capabilities`，
-- 没有 WHERE，走 PRIMARY KEY 覆盖扫描即可。行数量级是「provider × model」组合
-- （llmux 十几个 provider），几十行量级，二级索引纯属写放大。
-- 加索引前先 EXPLAIN QUERY PLAN —— 这条规矩对 usage_logs 已经吃过一次亏
-- （见 0024：10 个索引合计 14.0 MiB，而表本身 6.2 MiB，其中 6 个从未被任何计划选中）。
--
-- supported / rejected 存成逗号分隔的字符串，与观测层的内存表示一致。
-- 空串 = 该侧没有任何记录。两侧都空的行没有信息量，写入时会被跳过。
--
-- ⚠️ init_db **没有迁移记录表**：25 个迁移每次启动全部重跑，靠吞掉
-- 「already exists」假装幂等。CREATE TABLE IF NOT EXISTS 是幂等的，重跑无害。

CREATE TABLE IF NOT EXISTS reasoning_effort_capabilities (
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  supported TEXT NOT NULL DEFAULT '',
  rejected TEXT NOT NULL DEFAULT '',
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (provider, model)
);
