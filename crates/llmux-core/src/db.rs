use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};
use std::str::FromStr;

pub const INIT_SQL: &str = include_str!("migrations/0001_init.sql");
pub const MIGRATION_002: &str = include_str!("migrations/0002_add_account_ids.sql");
pub const MIGRATION_003: &str = include_str!("migrations/0003_add_openai_compatible.sql");
pub const MIGRATION_004: &str = include_str!("migrations/0004_add_preferred_account_id.sql");
pub const MIGRATION_005: &str = include_str!("migrations/0005_add_account_model_cache.sql");
pub const MIGRATION_006: &str = include_str!("migrations/0006_perf_indexes.sql");
pub const MIGRATION_007: &str = include_str!("migrations/0007_add_aggregate_aliases.sql");
pub const MIGRATION_008: &str = include_str!("migrations/0008_add_upstream_api.sql");
pub const MIGRATION_0009: &str = include_str!("migrations/0009_account_endpoints.sql");
pub const MIGRATION_0010: &str = include_str!("migrations/0010_add_usage_log_bodies.sql");
pub const MIGRATION_0011: &str = include_str!("migrations/0011_add_usage_log_client_ip.sql");
pub const MIGRATION_0012: &str = include_str!("migrations/0012_sync_chat_endpoint_to_base_url.sql");
pub const MIGRATION_0013: &str = include_str!("migrations/0013_add_timing_metrics.sql");
pub const MIGRATION_0014: &str = include_str!("migrations/0014_add_health_index.sql");
pub const MIGRATION_0015: &str = "ALTER TABLE accounts ADD COLUMN balance_provider TEXT NOT NULL DEFAULT '';";
pub const MIGRATION_0016: &str = "ALTER TABLE accounts ADD COLUMN balance_auth TEXT NOT NULL DEFAULT '';";
pub const MIGRATION_0017: &str = include_str!("migrations/0017_account_activity_index.sql");
pub const MIGRATION_0018: &str = include_str!("migrations/0018_model_protocol_cache.sql");
pub const MIGRATION_0019: &str = include_str!("migrations/0019_model_test_results.sql");
pub const MIGRATION_0020: &str = include_str!("migrations/0020_merge_protocol_cache.sql");
pub const MIGRATION_0021: &str = include_str!("migrations/0021_model_probe_suspension.sql");
pub const MIGRATION_0022: &str = include_str!("migrations/0022_admin_credentials.sql");
pub const MIGRATION_0023: &str = include_str!("migrations/0023_probe_suspension_traffic_cooldown.sql");
pub const MIGRATION_0024: &str = include_str!("migrations/0024_prune_unused_usage_log_indexes.sql");
pub const MIGRATION_0025: &str = include_str!("migrations/0025_reasoning_effort_capabilities.sql");
pub const MIGRATION_0026: &str = include_str!("migrations/0026_usage_log_api_key.sql");
pub const MIGRATION_0027: &str = include_str!("migrations/0027_model_prices_cost.sql");

pub async fn connect_sqlite(database_url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        .busy_timeout(std::time::Duration::from_secs(5))
        .foreign_keys(true)
        .optimize_on_close(true, None);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .min_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect_with(options)
        .await?;
    Ok(pool)
}

pub async fn init_db(pool: &SqlitePool) -> Result<()> {
    for statement in INIT_SQL.split(';') {
        let statement = statement.trim();
        if !statement.is_empty() {
            pool.execute(statement).await?;
        }
    }
    // Run migrations (ignore errors for already-applied statements)
    let migrations = [
        ("0002", MIGRATION_002),
        ("0003", MIGRATION_003),
        ("0004", MIGRATION_004),
        ("0005", MIGRATION_005),
        ("0006", MIGRATION_006),
        ("0007", MIGRATION_007),
        ("0008", MIGRATION_008),
        ("0009", MIGRATION_0009),
        ("0010", MIGRATION_0010),
        ("0011", MIGRATION_0011),
        ("0012", MIGRATION_0012),
        ("0013", MIGRATION_0013),
        ("0014", MIGRATION_0014),
        ("0015", MIGRATION_0015),
        ("0016", MIGRATION_0016),
        ("0017", MIGRATION_0017),
        ("0018", MIGRATION_0018),
        ("0019", MIGRATION_0019),
        ("0020", MIGRATION_0020),
        ("0021", MIGRATION_0021),
        ("0022", MIGRATION_0022),
        ("0023", MIGRATION_0023),
        ("0024", MIGRATION_0024),
        ("0025", MIGRATION_0025),
        ("0026", MIGRATION_0026),
        ("0027", MIGRATION_0027),
    ];
    for (name, sql) in &migrations {
        for statement in sql.split(';') {
            let statement = statement.trim();
            if !statement.is_empty() {
                match pool.execute(statement).await {
                    Ok(_) => {}
                    Err(e) => {
                        // SQLite "duplicate column" errors are expected for already-applied
                        // migrations. Log unexpected errors at warn level.
                        let msg = e.to_string();
                        if msg.contains("duplicate column") || msg.contains("already exists") {
                            tracing::debug!("Migration {name} already applied: {msg}");
                        } else {
                            tracing::warn!("Migration {name} statement failed: {msg}");
                        }
                    }
                }
            }
        }
    }
    assert_suspension_columns(pool).await;
    Ok(())
}

/// 0023 加的两列是「探活失败不挡生产流量」这层保护的地基，而上面的迁移循环会
/// 吞掉所有非「duplicate column」错误。于是 ALTER 一旦失败（库只读、被锁、
/// 磁盘满），服务照常启动，而 `is_traffic_suspended` 的 `.ok().flatten()` 把查询
/// 错误变成「未冷却」—— 看着一切正常，实际是配额冷却整个失效。
///
/// 这里只**大声告警**，不阻断启动：改 init_db 的错误语义会波及全部 23 个迁移，
/// 任何一个在某个部署上出岔子都会让网关起不来，那是更大的事故。schema 由
/// `core_contract::migration_0023_*` 钉在 CI 上，这里负责线上可诊断。
async fn assert_suspension_columns(pool: &SqlitePool) {
    for col in ["traffic_suspended_until", "consecutive_quota_failures"] {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('model_probe_suspensions') WHERE name = ?",
        )
        .bind(col)
        .fetch_one(pool)
        .await
        .unwrap_or(-1);
        if n != 1 {
            tracing::error!(
                "🔴 迁移 0023 未生效：model_probe_suspensions.{col} 缺失。\
                 配额冷却将整个失效（上游 429 会被反复重打）。请检查数据库是否可写。"
            );
        }
    }
}

pub fn sqlite_url_from_path(path: &std::path::Path) -> String {
    format!("sqlite://{}", path.display().to_string().replace('\\', "/"))
}
