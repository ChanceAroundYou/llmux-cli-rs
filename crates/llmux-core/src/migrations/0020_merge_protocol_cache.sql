-- 合并 model_protocol_cache（0018）进本表。
--
-- 0018 的设计是「每个可用协议一行」，但主键写成了 (account_id, model)、漏了
-- protocol —— 同一个 (account_id, model) 物理上只能存一行，多协议模型的第二次
-- INSERT 必然撞 UNIQUE 回滚。并行探测上线后立刻暴露：线上日志 106 次
-- "UNIQUE constraint failed: model_protocol_cache.account_id, model_protocol_cache.model"，
-- 表里 14 行全是单协议（多协议的一个都没存进去）。SQLite 不能 ALTER 主键，
-- 而这张表存的东西（协议集合 + 时间戳）本就是本表 supported/checked_at 的子集，
-- 所以不修、直接并进来。
--
-- source 标记写入方，**不是可选的**：聚合探活每 300s 一轮，没有它就会把用户
-- 手工拨测的结果覆盖掉。health 的「最近一次状态」只认 manual，角标三源通用。
--   manual    —— UI 拨测按钮 / 单模型拨测
--   aggregate —— 聚合别名后台探活
--   verify    —— 保存别名时的自动校验
ALTER TABLE model_test_results ADD COLUMN source TEXT NOT NULL DEFAULT 'manual';

DROP TABLE IF EXISTS model_protocol_cache;
