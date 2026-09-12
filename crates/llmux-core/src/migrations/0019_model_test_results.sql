-- 拨测结果与真实流量分开记录：health 面板要能同时回答「拨测通不通」和
-- 「最近真实调用成不成」，而此前 usage_logs 按 (account, model) 取最新一条时
-- 两者互相覆盖（真实流量随时刷新，把拨测结果冲掉）。
--
-- 记录分开存（各自一张表、各自的时间戳），展示时再合并 —— 这样任一方更新都
-- 不会抹掉另一方，同时 UI 仍能在一行里给出「最近一次状态」。
CREATE TABLE IF NOT EXISTS model_test_results (
  account_id INTEGER NOT NULL,
  model TEXT NOT NULL,
  success INTEGER NOT NULL,
  latency_ms INTEGER NOT NULL,
  error_message TEXT,
  via TEXT,
  supported TEXT,
  checked_at INTEGER NOT NULL,
  PRIMARY KEY (account_id, model)
);
