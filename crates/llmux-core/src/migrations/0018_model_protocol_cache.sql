-- 探测到的「(账户, 模型) 能走通哪个协议」缓存。
-- 只作事实记录 + 下次探测起点 + UI 角标，**不参与路由** —— 线上走哪个协议
-- 完全由 alias/aggregate 的 upstream_api 决定，这里探出不一致时只提示
-- （配置写错是用户的事，不静默替他改）。
CREATE TABLE IF NOT EXISTS model_protocol_cache (
  account_id INTEGER NOT NULL,
  model TEXT NOT NULL,
  protocol TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (account_id, model)
);
