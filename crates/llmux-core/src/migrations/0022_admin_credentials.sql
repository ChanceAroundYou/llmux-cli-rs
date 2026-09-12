-- 管理员登录凭据。此前用户名/密码是 auth.rs 里的硬编码 fallback，既进了
-- git 历史，也在公开仓库里躺过一段时间（v0.3.3 起移除，历史已重写）。
--
-- 不放 settings 表：`/api/settings` 会整表回吐，`export_config` 也会把
-- settings 全量导出成可下载的 JSON —— 密码哈希放进去等于随导出文件外流。
-- 独立单行表，两个路径都碰不到。
--
-- 无行时回落到 env（ADMIN_USERNAME/ADMIN_PASSWORD），再回落到 admin/admin。
-- 只要 UI 改过一次，本表就是唯一事实来源（env 不再覆盖，否则前端改了不生效）。
CREATE TABLE IF NOT EXISTS admin_credentials (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  username TEXT NOT NULL,
  -- scrypt 加盐哈希，格式 v1:<salt_b64>:<hash_b64>。不存明文、不可逆。
  password_hash TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
