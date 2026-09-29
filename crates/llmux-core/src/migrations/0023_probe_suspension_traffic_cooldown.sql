-- 探活失败与真实配额 429 曾共用 suspended_until 一列，两个后果都很糟：
--
--   * 上游把某模型下架（但仍留在 /v1/models）导致探活连败 2 次 → suspended_until
--     被写满 30 分钟 → 生产流量被 `rate_limit_suspended` 挡 30 分钟，还回
--     429 + Retry-After，尽管账户配额充足。probe.rs 的模块注释写的是
--     「只拦自动拨测」，代码并没有做到。
--   * 反向：openai.rs 的 `dispatch_aggregate_openai`（聚合别名的直通路径）
--     只写冷却、从不读，绕过了这层保护。
--
-- 拆成两列冷却 + 两个计数器，各记各的：
--
--   suspended_until / consecutive_failures
--       探活侧 —— is_suspended() 读（后台探活 + 批量队列 + UI 角标）
--   traffic_suspended_until / consecutive_quota_failures
--       流量侧 —— is_traffic_suspended() 读（真实请求）
--
-- 计数器也必须分开：只拆冷却列而共用计数的话，「1 次探活失败 + 1 次配额 429」
-- 凑够阈值，单次配额 429 就会开挡 —— 那正是要修的东西又从后门回来了。
--
-- 配额类 429 两侧都记（真没配额了，再探也是白花钱）；探活失败只动探活侧；
-- 任一成功（探活或真实流量）都走 clear_suspension 整条 DELETE。
--
-- 默认 0 = 无流量侧冷却 = 不挡生产流量。对存量行这是安全默认值：那条暂停
-- 可能正是被探活失败写出来的，部署瞬间武装它等于把老 bug 固化下来。代价是
-- 已耗尽配额的账户多吃一两个 429，它下次一 429 就重新计数开挡。
ALTER TABLE model_probe_suspensions
  ADD COLUMN traffic_suspended_until INTEGER NOT NULL DEFAULT 0;
ALTER TABLE model_probe_suspensions
  ADD COLUMN consecutive_quota_failures INTEGER NOT NULL DEFAULT 0;
