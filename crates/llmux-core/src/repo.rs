//! 数据访问层（repo）：集中封装 vendors / accounts / api_keys / model_aliases
//! 的全部 SQL，路由与测试不再直接写裸 sqlx 查询。
//!
//! 约定：所有函数为自由异步函数，第一个参数为 `&SqlitePool`；
//! 需要原子性的复合操作（如“替换白名单”）在函数内部自建事务。

use crate::models::{Account, AccountPublic, ApiKey, ModelAlias, Vendor};
use anyhow::Result;
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// vendors
// ---------------------------------------------------------------------------

/// 列出全部厂商：内置优先、id 升序。
pub async fn list_vendors(pool: &SqlitePool) -> Result<Vec<Vendor>> {
    let rows = sqlx::query(
        "SELECT id, name, protocol, protocols, openai_responses, default_base_url, default_anthropic_url, builtin, created_at
         FROM vendors ORDER BY builtin DESC, id",
    )
    .fetch_all(pool)
    .await?;
    let mut vendors = Vec::with_capacity(rows.len());
    for row in rows {
        let protocols_raw: String = row.try_get("protocols").unwrap_or_default();
        vendors.push(Vendor {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            protocol: row.try_get("protocol")?,
            protocols: serde_json::from_str(&protocols_raw).unwrap_or_default(),
            openai_responses: row.try_get::<i64, _>("openai_responses").unwrap_or(1) == 1,
            default_base_url: row.try_get("default_base_url")?,
            default_anthropic_url: row.try_get("default_anthropic_url")?,
            builtin: row.try_get("builtin").unwrap_or(0),
            created_at: row.try_get("created_at")?,
        });
    }
    Ok(vendors)
}

/// 新建厂商（builtin 固定为 0，用户自建）。
#[allow(clippy::too_many_arguments)]
pub async fn create_vendor(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    protocol: &str,
    protocols: &[String],
    openai_responses: bool,
    default_base_url: Option<&str>,
    default_anthropic_url: Option<&str>,
) -> Result<()> {
    let protocols_json = serde_json::to_string(protocols).unwrap_or_default();
    sqlx::query(
        "INSERT INTO vendors (id, name, protocol, protocols, openai_responses, default_base_url, default_anthropic_url, builtin)
         VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
    )
    .bind(id)
    .bind(name)
    .bind(protocol)
    .bind(protocols_json)
    .bind(if openai_responses { 1 } else { 0 })
    .bind(default_base_url)
    .bind(default_anthropic_url)
    .execute(pool)
    .await?;
    Ok(())
}

/// 更新厂商，返回受影响行数（0 = 未找到）。
#[allow(clippy::too_many_arguments)]
pub async fn update_vendor(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    protocol: &str,
    protocols: &[String],
    openai_responses: bool,
    default_base_url: Option<&str>,
    default_anthropic_url: Option<&str>,
) -> Result<u64> {
    let protocols_json = serde_json::to_string(protocols).unwrap_or_default();
    let result = sqlx::query(
        "UPDATE vendors SET name = ?, protocol = ?, protocols = ?, openai_responses = ?, default_base_url = ?, default_anthropic_url = ? WHERE id = ?",
    )
    .bind(name)
    .bind(protocol)
    .bind(protocols_json)
    .bind(if openai_responses { 1 } else { 0 })
    .bind(default_base_url)
    .bind(default_anthropic_url)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 删除厂商，返回受影响行数（0 = 未找到）。
pub async fn delete_vendor(pool: &SqlitePool, id: &str) -> Result<u64> {
    let result = sqlx::query("DELETE FROM vendors WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 查询厂商协议与默认 base_url（用于账户校验与自动填 URL）。
pub async fn get_vendor(pool: &SqlitePool, id: &str) -> Result<Option<(String, Option<String>)>> {
    let row = sqlx::query("SELECT protocol, default_base_url FROM vendors WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| {
        (
            r.try_get::<String, _>("protocol").unwrap_or_default(),
            r.try_get::<Option<String>, _>("default_base_url")
                .unwrap_or_default(),
        )
    }))
}

// ---------------------------------------------------------------------------
// accounts
// ---------------------------------------------------------------------------

/// 列出对外可见的账户视图（不含 api_key_enc），id 降序。
pub async fn list_accounts_public(pool: &SqlitePool) -> Result<Vec<AccountPublic>> {
    Ok(sqlx::query_as::<_, AccountPublic>(
        "SELECT id, vendor_id, name, base_url, anthropic_base_url, openai_compatible, CAST(enabled AS INTEGER) as enabled, weight, notes, created_at FROM accounts ORDER BY id DESC",
    )
    .fetch_all(pool)
    .await?)
}

/// 新建账户，返回 last_insert_rowid。
#[allow(clippy::too_many_arguments)]
pub async fn create_account(
    pool: &SqlitePool,
    vendor_id: &str,
    name: &str,
    api_key_enc: &str,
    base_url: Option<&str>,
    anthropic_base_url: Option<&str>,
    openai_compatible: i64,
    enabled: i64,
    weight: i64,
    notes: Option<&str>,
) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO accounts (vendor_id, name, api_key_enc, base_url, anthropic_base_url, openai_compatible, enabled, weight, notes)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(vendor_id)
    .bind(name)
    .bind(api_key_enc)
    .bind(base_url)
    .bind(anthropic_base_url)
    .bind(openai_compatible)
    .bind(enabled)
    .bind(weight)
    .bind(notes)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// 按 id 读取账户全列。
pub async fn get_account(pool: &SqlitePool, id: i64) -> Result<Option<Account>> {
    Ok(sqlx::query_as::<_, Account>(
        "SELECT id, vendor_id, name, api_key_enc, base_url, anthropic_base_url, openai_compatible, enabled, weight, notes, limits_cache, limits_cache_updated_at, created_at FROM accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// 更新账户，返回受影响行数。
#[allow(clippy::too_many_arguments)]
pub async fn update_account(
    pool: &SqlitePool,
    id: i64,
    vendor_id: &str,
    name: &str,
    api_key_enc: &str,
    base_url: Option<&str>,
    anthropic_base_url: Option<&str>,
    openai_compatible: i64,
    enabled: i64,
    weight: i64,
    notes: Option<&str>,
) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE accounts SET vendor_id = ?, name = ?, api_key_enc = ?, base_url = ?, anthropic_base_url = ?, openai_compatible = ?, enabled = ?, weight = ?, notes = ? WHERE id = ?",
    )
    .bind(vendor_id)
    .bind(name)
    .bind(api_key_enc)
    .bind(base_url)
    .bind(anthropic_base_url)
    .bind(openai_compatible)
    .bind(enabled)
    .bind(weight)
    .bind(notes)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 删除账户，返回受影响行数。
pub async fn delete_account(pool: &SqlitePool, id: i64) -> Result<u64> {
    let result = sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 按厂商 + 名称查找账户 id（web session 复用用）。
pub async fn find_account_by_vendor_and_name(
    pool: &SqlitePool,
    vendor_id: &str,
    name: &str,
) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT id FROM accounts WHERE vendor_id = ? AND name = ?",
    )
    .bind(vendor_id)
    .bind(name)
    .fetch_optional(pool)
    .await?)
}

/// 更新账户密钥密文，返回受影响行数。
pub async fn set_account_api_key_enc(pool: &SqlitePool, id: i64, api_key_enc: &str) -> Result<u64> {
    let result = sqlx::query("UPDATE accounts SET api_key_enc = ? WHERE id = ?")
        .bind(api_key_enc)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// api_keys
// ---------------------------------------------------------------------------

/// 列出全部网关 key（含明文），id 升序。
pub async fn list_api_keys(pool: &SqlitePool) -> Result<Vec<ApiKey>> {
    Ok(sqlx::query_as::<_, ApiKey>(
        "SELECT id, name, key, enabled, last_used_at, created_at FROM api_keys ORDER BY id",
    )
    .fetch_all(pool)
    .await?)
}

/// 新建网关 key，返回 last_insert_rowid。
pub async fn create_api_key(pool: &SqlitePool, name: &str, key: &str) -> Result<i64> {
    let result = sqlx::query("INSERT INTO api_keys (name, key) VALUES (?, ?)")
        .bind(name)
        .bind(key)
        .execute(pool)
        .await?;
    Ok(result.last_insert_rowid())
}

/// 更新网关 key 名称，返回受影响行数。
pub async fn update_api_key_name(pool: &SqlitePool, id: i64, name: &str) -> Result<u64> {
    let result = sqlx::query("UPDATE api_keys SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 删除网关 key（白名单外键 CASCADE 自动清空），返回受影响行数。
pub async fn delete_api_key(pool: &SqlitePool, id: i64) -> Result<u64> {
    let result = sqlx::query("DELETE FROM api_keys WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 更新网关 key 最近使用时间（鉴权通过时调用）。
pub async fn update_api_key_last_used(pool: &SqlitePool, id: i64) -> Result<u64> {
    // 60 秒窗口内不重复写：避免每个请求都触发一次 SQLite 写（鉴权热路径）。
    // WHERE 里对 last_used_at 与 datetime('now',...) 比较，格式同为 UTC 'YYYY-MM-DD HH:MM:SS'，字符串比较有效。
    let result = sqlx::query(
        "UPDATE api_keys SET last_used_at = CURRENT_TIMESTAMP
         WHERE id = ? AND (last_used_at IS NULL OR last_used_at < datetime('now', '-60 seconds'))",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 读取一个 key 的模型白名单（空表 = 不限制）。
pub async fn list_key_models(pool: &SqlitePool, key_id: i64) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT model FROM api_key_models WHERE api_key_id = ?")
        .bind(key_id)
        .fetch_all(pool)
        .await?)
}

/// 替换一个 key 的模型白名单（先清空再写入，内部事务保证原子性）。
pub async fn replace_key_models(pool: &SqlitePool, key_id: i64, models: &[String]) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM api_key_models WHERE api_key_id = ?")
        .bind(key_id)
        .execute(&mut *tx)
        .await?;
    for model in models {
        sqlx::query("INSERT OR IGNORE INTO api_key_models (api_key_id, model) VALUES (?, ?)")
            .bind(key_id)
            .bind(model)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 按明文 key 查找启用的网关 key，返回 (id, name, enabled)（鉴权用）。
pub async fn find_api_key_by_value(
    pool: &SqlitePool,
    key: &str,
) -> Result<Option<(i64, String, i64)>> {
    Ok(sqlx::query_as::<_, (i64, String, i64)>(
        "SELECT id, name, enabled FROM api_keys WHERE key = ? AND enabled = 1",
    )
    .bind(key)
    .fetch_optional(pool)
    .await?)
}

/// 一次查询返回启用的网关 key 及其模型白名单：(id, name, models)。
/// 鉴权热路径用：把 key 查找 + 白名单查询合并为单条 LEFT JOIN，避免每次请求两次往返。
pub async fn find_api_key_with_models(
    pool: &SqlitePool,
    key: &str,
) -> Result<Option<(i64, String, Vec<String>)>> {
    let rows = sqlx::query(
        "SELECT k.id AS id, k.name AS name, m.model AS model
         FROM api_keys k
         LEFT JOIN api_key_models m ON m.api_key_id = k.id
         WHERE k.key = ? AND k.enabled = 1
         ORDER BY k.id",
    )
    .bind(key)
    .fetch_all(pool)
    .await?;

    let mut first: Option<(i64, String, Vec<String>)> = None;
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let name: String = row.try_get("name")?;
        let model: Option<String> = row.try_get("model")?;
        match &mut first {
            // 同 key 多行白名单时追加（空白名单为 NULL，产生空 Vec）
            Some((cur_id, _, models)) if *cur_id == id => {
                if let Some(m) = model {
                    models.push(m);
                }
            }
            _ => {
                first = Some((id, name, model.into_iter().collect()));
            }
        }
    }
    Ok(first)
}

// ---------------------------------------------------------------------------
// model_aliases
// ---------------------------------------------------------------------------

/// 列出全部 alias，id 升序。
pub async fn list_aliases(pool: &SqlitePool) -> Result<Vec<ModelAlias>> {
    Ok(sqlx::query_as::<_, ModelAlias>(
        "SELECT id, alias, target_model, vendor_id, created_at FROM model_aliases ORDER BY id",
    )
    .fetch_all(pool)
    .await?)
}

/// 按 id 读取 alias 名称（删 alias 时用于清理悬挂白名单）。
pub async fn get_alias_name_by_id(pool: &SqlitePool, id: i64) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT alias FROM model_aliases WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

/// 写入 alias（按 alias 唯一键 UPSERT），返回真实 id。
/// 注意：UPSERT 触发 UPDATE 分支时 last_insert_rowid() 不会被 SQLite 更新，
/// 因此必须用 RETURNING id 取回（否则编辑已有 alias 时绑定会引用错误 id）。
pub async fn upsert_alias(
    pool: &SqlitePool,
    alias: &str,
    target_model: &str,
    vendor_id: Option<&str>,
) -> Result<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO model_aliases (alias, target_model, vendor_id)
         VALUES (?, ?, ?)
         ON CONFLICT(alias) DO UPDATE SET target_model = excluded.target_model, vendor_id = excluded.vendor_id
         RETURNING id",
    )
    .bind(alias)
    .bind(target_model)
    .bind(vendor_id)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// 删除 alias（绑定行外键 CASCADE 自动清空），返回受影响行数。
pub async fn delete_alias(pool: &SqlitePool, id: i64) -> Result<u64> {
    let result = sqlx::query("DELETE FROM model_aliases WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 读取一个 alias 的绑定账户集（position 升序），返回 (account_id, is_preferred)。
pub async fn list_alias_bindings(pool: &SqlitePool, alias_id: i64) -> Result<Vec<(i64, i64)>> {
    Ok(sqlx::query_as::<_, (i64, i64)>(
        "SELECT account_id, is_preferred FROM model_alias_accounts
         WHERE alias_id = ? ORDER BY position, id",
    )
    .bind(alias_id)
    .fetch_all(pool)
    .await?)
}

/// 替换一个 alias 的绑定集（先清空再写入，内部事务保证原子性）。
pub async fn replace_alias_bindings(
    pool: &SqlitePool,
    alias_id: i64,
    account_ids: &[i64],
    preferred: Option<i64>,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM model_alias_accounts WHERE alias_id = ?")
        .bind(alias_id)
        .execute(&mut *tx)
        .await?;
    for (position, account_id) in account_ids.iter().enumerate() {
        let is_preferred = if Some(*account_id) == preferred { 1 } else { 0 };
        sqlx::query(
            "INSERT INTO model_alias_accounts (alias_id, account_id, position, is_preferred)
             VALUES (?, ?, ?, ?)",
        )
        .bind(alias_id)
        .bind(account_id)
        .bind(position as i64)
        .bind(is_preferred)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// alias 绑定账户行 + 账户/厂商信息（供返回形状直接使用）。
#[derive(Debug, Clone)]
pub struct AliasBindingRow {
    pub account_id: i64,
    pub account_name: String,
    pub vendor_id: String,
    pub vendor_name: String,
    pub protocol: String,
    pub is_preferred: i64,
}

/// 批量取多个 alias 的绑定账户 + 账户/厂商信息，按 alias_id 分组。
/// 一次查询消除 N+1；alias_ids 为空时返回空 map。
pub async fn list_alias_bindings_with_vendors(
    pool: &SqlitePool,
    alias_ids: &[i64],
) -> Result<HashMap<i64, Vec<AliasBindingRow>>> {
    let mut map: HashMap<i64, Vec<AliasBindingRow>> = HashMap::new();
    if alias_ids.is_empty() {
        return Ok(map);
    }
    let placeholders: Vec<String> = alias_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT b.alias_id, b.account_id, b.is_preferred,
                a.vendor_id, a.name AS account_name, v.name AS vendor_name, v.protocol
         FROM model_alias_accounts b
         JOIN accounts a ON a.id = b.account_id
         JOIN vendors v ON v.id = a.vendor_id
         WHERE b.alias_id IN ({})
         ORDER BY b.alias_id, b.position, b.id",
        placeholders.join(",")
    );
    let mut query = sqlx::query(&sql);
    for id in alias_ids {
        query = query.bind(id);
    }
    let rows = query.fetch_all(pool).await?;
    for row in rows {
        let alias_id: i64 = row.try_get("alias_id")?;
        let entry = map.entry(alias_id).or_default();
        entry.push(AliasBindingRow {
            account_id: row.try_get("account_id")?,
            account_name: row.try_get("account_name")?,
            vendor_id: row.try_get("vendor_id")?,
            vendor_name: row.try_get("vendor_name")?,
            protocol: row.try_get("protocol")?,
            is_preferred: row.try_get::<i64, _>("is_preferred").unwrap_or(0),
        });
    }
    Ok(map)
}


// ---------------------------------------------------------------------------
// 查询专用：无对应 model 类型时用轻量结构/元组承载
// ---------------------------------------------------------------------------

/// 账户健康检查用：列出 (id, name)。
pub async fn list_account_id_name(pool: &SqlitePool) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query_as::<_, (i64, String)>(
        "SELECT id, name FROM accounts ORDER BY id",
    )
    .fetch_all(pool)
    .await?)
}

/// 账户最近成功率统计：返回 (total, success_count)；无记录时 SUM 为 NULL → 记 0。
pub async fn get_account_usage_stats(
    pool: &SqlitePool,
    account_id: i64,
) -> Result<Option<(i64, i64)>> {
    let row = sqlx::query(
        "SELECT COUNT(*) as total,
                SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as success
         FROM usage_logs
         WHERE account_id = ?",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| {
        let total: i64 = r.try_get("total").unwrap_or_default();
        let success: i64 = r.try_get("success").unwrap_or_default();
        (total, success)
    }))
}

/// 账户用量导出用：列出 (ts, model, latency_ms, success, error_message)，ts 降序。
pub async fn list_account_usage_logs(
    pool: &SqlitePool,
    account_id: i64,
) -> Result<Vec<(i64, Option<String>, i64, i64, Option<String>)>> {
    Ok(sqlx::query_as::<_, (i64, Option<String>, i64, i64, Option<String>)>(
        "SELECT ts, model, latency_ms, success, error_message FROM usage_logs WHERE account_id = ? ORDER BY ts DESC",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?)
}

/// 仪表盘活动流一行（无 token 明细）。
#[derive(Debug, sqlx::FromRow)]
pub struct ActivityEntry {
    pub id: i64,
    pub ts: i64,
    pub model: Option<String>,
    pub success: i64,
    pub latency_ms: i64,
    pub error_message: Option<String>,
    pub account_name: Option<String>,
}

/// 最近活动流：按时间倒序取 limit 条。
pub async fn list_recent_activity(pool: &SqlitePool, limit: i64) -> Result<Vec<ActivityEntry>> {
    Ok(sqlx::query_as::<_, ActivityEntry>(
        "SELECT l.id, l.ts, l.model, l.success, l.latency_ms,
                l.error_message, l.account_name
         FROM usage_logs l
         ORDER BY l.ts DESC
         LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// 模型健康检查一行（最新一条 usage_log 按 (account_id, model) 分组）。
#[derive(Debug, sqlx::FromRow)]
pub struct ModelHealthRow {
    pub account_id: Option<i64>,
    pub vendor_id: Option<String>,
    pub model: Option<String>,
    pub last_checked: Option<i64>,
    pub success: Option<i64>,
    pub latency: Option<i64>,
    pub error: Option<String>,
    pub limits_cache: Option<String>,
    pub limits_cache_updated_at: Option<String>,
    pub account_name: Option<String>,
}

/// 模型健康检查：分组取最新记录，JOIN vendors/accounts 解析厂商与缓存。
pub async fn get_model_health(pool: &SqlitePool) -> Result<Vec<ModelHealthRow>> {
    Ok(sqlx::query_as::<_, ModelHealthRow>(
        "SELECT u.account_id, v.id AS vendor_id, u.model, u.ts AS last_checked, \
                u.success, u.latency_ms AS latency, u.error_message AS error, \
                a.limits_cache, a.limits_cache_updated_at, u.account_name \
         FROM usage_logs u \
         LEFT JOIN accounts a ON u.account_id = a.id \
         LEFT JOIN vendors v ON a.vendor_id = v.id \
         WHERE u.id IN ( \
           SELECT MAX(id) FROM usage_logs GROUP BY account_id, model \
         )",
    )
    .fetch_all(pool)
    .await?)
}

/// 自定义模型清单：alias 的目标模型去重（供 /models/available 合并）。
pub async fn list_alias_custom_models(pool: &SqlitePool) -> Result<Vec<(String, Option<String>)>> {
    Ok(sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT DISTINCT target_model, vendor_id FROM model_aliases \
         WHERE target_model IS NOT NULL AND target_model != ''",
    )
    .fetch_all(pool)
    .await?)
}
