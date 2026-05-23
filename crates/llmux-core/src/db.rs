use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};
use std::str::FromStr;

pub const INIT_SQL: &str = include_str!("migrations/0001_init.sql");
pub const MIGRATION_002: &str = include_str!("migrations/0002_add_account_ids.sql");

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
    // Run migrations (ignore errors for already-applied migrations)
    for statement in MIGRATION_002.split(';') {
        let statement = statement.trim();
        if !statement.is_empty() {
            let _ = pool.execute(statement).await;
        }
    }
    Ok(())
}

pub fn sqlite_url_from_path(path: &std::path::Path) -> String {
    format!("sqlite://{}", path.display().to_string().replace('\\', "/"))
}
