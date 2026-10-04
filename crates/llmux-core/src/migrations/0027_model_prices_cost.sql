-- 费用估算的地基：把 0001 就建好、但一直 0 行且无人读写的 model_prices 用起来。
--
-- 单位是 **美元 / token**（OpenRouter 报价的原始单位），不是「每百万 token」。
-- 读取侧只做「token 数 × 单价」的乘加，不改单位，避免存储与展示两处口径打架。
--
-- source 是本迁移的关键：自动刷新只覆盖 'openrouter' 行，'manual' 行（手工填的、
-- 以及免费/本地模型记 0 的行）永不被冲掉。这正是先前考虑另建一张
-- model_price_cache 表的唯一理由 —— 一列 `source` 就够，不拆表。
--
-- source_model_id 记「这个网关模型名匹配到了哪个 OpenRouter id」，用于溯源；
-- model_id 始终是网关侧模型名（= usage_logs.model），这样统计查询能直接按 model 取值。
--
-- 不加索引：model_prices 是小表（几十行），读取侧按主键 model_id 命中。

ALTER TABLE model_prices ADD COLUMN cache_read_price REAL;
ALTER TABLE model_prices ADD COLUMN cache_write_price REAL;
ALTER TABLE model_prices ADD COLUMN source TEXT NOT NULL DEFAULT 'openrouter';
ALTER TABLE model_prices ADD COLUMN source_model_id TEXT;

-- 2026-10 现状快照：OpenRouter 公开渠道查不到报价的模型，手工记 0。
-- deepseek-flash 是 go6 的失败空记录（0 token）；其余是订阅制自造名或本地模型。
-- 「记 0」的前提是这些上游按包月 / 本地计费，不是按 token；将来若改成按量计费，
-- 把对应行的 source 改成 openrouter（允许刷新）或手工改价即可。
-- 本地 GGUF（名字以 .gguf 结尾）不在此列：没有价行时读取侧本来就记 0。
INSERT OR IGNORE INTO model_prices
  (model_id, vendor, input_price, output_price, cache_read_price, cache_write_price, source, source_model_id)
VALUES
  ('omen-alpha',          'opencode-zen', 0, 0, 0, 0, 'manual', NULL),
  ('agnes-3.0-flash',     'agnes',        0, 0, 0, 0, 'manual', NULL),
  ('agnes-2.5-flash',     'agnes',        0, 0, 0, 0, 'manual', NULL),
  ('deepseek-flash-free', 'teamorouter',  0, 0, 0, 0, 'manual', NULL),
  ('deepseek-flash',      'opencode-zen', 0, 0, 0, 0, 'manual', NULL),
  ('bonsai2-27b',         'local',        0, 0, 0, 0, 'manual', NULL),
  ('qwen3.8-max',         'dashscope',    0, 0, 0, 0, 'manual', NULL);
