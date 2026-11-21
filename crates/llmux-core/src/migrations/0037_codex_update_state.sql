-- 运行期状态：Codex 模型清单的客户端版本发现/探测结果。
--
-- 刻意不放进 settings 表：GET/PUT /api/settings 是整表回吐/合并的配置面，
-- 前端设置页保存时会把整份 localConfig 写回；如果运行期元数据混在里面，
-- 一次页面保存就可能把「刚探测通过」的版本状态覆盖成旧值，export_config
-- 也会把它当成用户配置导出。
--
-- 单行表（id = 1），只存「上一次探测/应用」的结果；用户偏好仍走 settings。
CREATE TABLE IF NOT EXISTS codex_update_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  latest_version TEXT,
  last_good_version TEXT,
  source TEXT,
  last_checked_at INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  pending_reason TEXT,
  auto_created_json TEXT NOT NULL DEFAULT '[]'
);
