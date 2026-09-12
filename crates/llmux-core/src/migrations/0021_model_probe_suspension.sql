-- 连续失败的模型自动暂停「自动拨测」，避免死模型每轮探活都刷一遍红点
-- （go5 有 7 个已被上游下架但仍在 /v1/models 里的模型，永远失败）。
--
-- 与 model_test_results 分开存：那张表是「最近一次结果」（UPSERT 覆盖），
-- 这张表是「连续失败计数 + 暂停到期时间」，两者生命周期不同 —— 成功一次就
-- 要清零重来，但成功的那次结果本身仍然要留在 model_test_results 里。
--
-- 只拦自动拨测（后台聚合探活 + 批量队列）。显式调用和单独拨测照常，且成功
-- 会清掉暂停（见 probe::clear_suspension）。
CREATE TABLE IF NOT EXISTS model_probe_suspensions (
  account_id INTEGER NOT NULL,
  model TEXT NOT NULL,
  -- 连续失败次数，成功即清零。
  consecutive_failures INTEGER NOT NULL DEFAULT 0,
  -- 暂停到期时间（毫秒）。为 0/过去表示未暂停。每次到期后再失败就 +30min。
  suspended_until INTEGER NOT NULL DEFAULT 0,
  -- 首次进入暂停的时间，仅用于展示「暂停多久了」。
  first_suspended_at INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  PRIMARY KEY (account_id, model)
);

-- health/角标要按 (account, model) 批量查暂停状态。
CREATE INDEX IF NOT EXISTS idx_probe_suspensions_until
  ON model_probe_suspensions(suspended_until);
