use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};
use std::str::FromStr;

pub const INIT_SQL: &str = include_str!("migrations/0001_init.sql");
pub const MIGRATION_002: &str = include_str!("migrations/0002_add_account_ids.sql");
pub const MIGRATION_003: &str = include_str!("migrations/0003_add_openai_compatible.sql");
pub const MIGRATION_004: &str = include_str!("migrations/0004_add_preferred_account_id.sql");

pub async fn connect_sqlite(database_url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
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
