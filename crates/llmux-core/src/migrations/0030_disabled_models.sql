-- 手动禁用的「(账户, 模型)」对 —— 与 model_protocol_cache 同粒度、同主键。
-- 用途分两层：
--   1. 账户 /v1/models 不播该模型（探测列表、llmux 的可用列表里消失）；
--   2. 别名/聚合绑定该模型时可直接判定为禁用，给出明确报错而不是上游 404。
-- 只记录事实，**不级联删别名**：用户可能故意禁掉上游模型、仍保留别名指向它，
-- 由前端决定改绑或删除。
CREATE TABLE IF NOT EXISTS disabled_models (
  account_id INTEGER NOT NULL,
  model TEXT NOT NULL,
  PRIMARY KEY (account_id, model)
);
