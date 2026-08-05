use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// 全新 schema（2026-08-05 spec）：vendors/accounts/model_aliases/model_alias_accounts/
/// api_keys/api_key_models/usage_logs/dispatch_state/app_settings。
pub const INIT_SQL: &str = include_str!("migrations/0001_init.sql");

pub async fn connect_sqlite(database_url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        // 外键是删账户 CASCADE / 删账户 SET NULL / 删厂商被挡 的前提
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    Ok(pool)
}

/// 初始化数据库。
///
/// 破坏性升级：0.3.x 旧 schema 与当前版本不兼容，不做数据迁移。检测到旧库时
/// 备份为 `<db>.legacy-<unix秒>.bak` 并重建全新空库，日志明确提示，原数据不迁移。
pub async fn init_db(pool: &mut SqlitePool, database_url: &str) -> Result<()> {
    if is_legacy_schema(pool).await? {
        let db_path = get_db_file_path(pool).await?;
        pool.close().await;
        let backup_path = backup_legacy_db(&db_path)?;
        tracing::warn!(
            "检测到旧版数据库 schema（0.3.x），本次升级为破坏性更新：旧库已备份到 {:?}，将重建空库，原数据不会被迁移。",
            backup_path
        );
        *pool = connect_sqlite(database_url).await?;
    }
    run_init_sql(pool).await?;
    Ok(())
}

async fn run_init_sql(pool: &SqlitePool) -> Result<()> {
    for statement in INIT_SQL.split(';') {
        let statement = statement.trim();
        if !statement.is_empty() {
            pool.execute(statement).await?;
        }
    }
    Ok(())
}

/// 旧 schema 检测：新库必有 `app_settings` 表；旧库（0.3.x）有 `settings`/`providers` 表。
/// 全新空文件无任何表 → 返回 false（按新库建表）。
async fn is_legacy_schema(pool: &SqlitePool) -> Result<bool> {
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(pool)
            .await?;
    if tables.iter().any(|t| t == "app_settings") {
        return Ok(false);
    }
    let has_legacy_tables = tables.iter().any(|t| t == "settings" || t == "providers");
    Ok(has_legacy_tables)
}

/// 取主库文件路径（pragma_database_list 的 file 列；内存库为空串）。
async fn get_db_file_path(pool: &SqlitePool) -> Result<PathBuf> {
    let (file,): (String,) =
        sqlx::query_as("SELECT file FROM pragma_database_list WHERE seq = 0")
            .fetch_one(pool)
            .await?;
    Ok(PathBuf::from(file))
}

/// 重命名旧库文件为 `<name>.legacy-<unix秒>.bak`，返回备份路径。
fn backup_legacy_db(db_path: &Path) -> Result<PathBuf> {
    if db_path.as_os_str().is_empty() {
        return Err(anyhow::anyhow!("无法定位数据库文件路径，跳过旧库备份"));
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let file_name = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("llmux_db.db");
    let backup = db_path.with_file_name(format!("{file_name}.legacy-{ts}.bak"));
    std::fs::rename(db_path, &backup)?;
    Ok(backup)
}

pub fn sqlite_url_from_path(path: &std::path::Path) -> String {
    format!("sqlite://{}", path.display().to_string().replace('\\', "/"))
}
