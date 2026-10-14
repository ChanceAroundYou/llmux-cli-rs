-- §A 手动 active 落库（C-A2/A3/A4）+ §B 锁定列（C-B1），一次建好。
-- active_idx：手动 set_active 的起点，重启后 ensure_entry 首次建 entry 时读回。
-- locked：1 = 整组探测与升降级暂停，直至解锁。默认 0。
-- 无迁移记录表，重启重跑靠 init_db 吞 duplicate column（见 AGENTS.md）。
ALTER TABLE aggregate_aliases ADD COLUMN active_idx INTEGER NOT NULL DEFAULT 0;
ALTER TABLE aggregate_aliases ADD COLUMN locked INTEGER NOT NULL DEFAULT 0;
