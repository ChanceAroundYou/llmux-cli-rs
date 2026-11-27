-- 性能自适应分流（Spill Valve）：默认关闭，逐个别名手动开。
-- 开启后，主候选（active）相对自身历史基线明显变慢时，把一部分新请求
-- 分流到下游候选；恢复后自动收回。不改变 active 指针与硬失败升降级语义。
ALTER TABLE aggregate_aliases ADD COLUMN adaptive_split INTEGER NOT NULL DEFAULT 0;
ALTER TABLE aggregate_aliases ADD COLUMN adaptive_max_spill_pct INTEGER NOT NULL DEFAULT 50;