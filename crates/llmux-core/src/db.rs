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
    Ok(())
}

pub fn sqlite_url_from_path(path: &std::path::Path) -> String {
    format!("sqlite://{}", path.display().to_string().replace('\\', "/"))
}
