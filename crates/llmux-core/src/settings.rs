use anyhow::Result;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use serde_json::{Map, Value};
use sqlx::SqlitePool;

#[derive(Debug, Clone)]
pub struct SettingsService {
    pool: SqlitePool,
}

impl SettingsService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get_all(&self) -> Result<Map<String, Value>> {
        let rows = sqlx::query_as::<_, (String, Option<String>)>("SELECT key, value FROM settings")
            .fetch_all(&self.pool)
            .await?;

        let mut result = Map::new();
        for (key, value) in rows {
            let value = match value {
                Some(raw) => serde_json::from_str::<Value>(&raw).unwrap_or(Value::String(raw)),
                None => Value::Null,
            };
            result.insert(key, value);
        }
        Ok(result)
    }

    pub async fn set(&self, key: &str, value: Value) -> Result<()> {
        let value = match value {
            Value::String(text) => text,
            other => serde_json::to_string(&other)?,
        };

        sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn batch_set(&self, settings: &Map<String, Value>) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (key, value) in settings {
            let value = match value {
                Value::String(text) => text.clone(),
                other => serde_json::to_string(other)?,
            };
            sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
                .bind(key)
                .bind(value)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_or_create_gateway_key(&self) -> Result<String> {
        if let Some(existing) =
            sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = 'gateway_key'")
                .fetch_optional(&self.pool)
                .await?
        {
            if !existing.is_empty() {
                return Ok(existing);
            }
        }

        let suffix: String = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(26)
            .map(char::from)
            .map(|ch| ch.to_ascii_lowercase())
            .collect();
        let key = format!("sk-llmux-{suffix}");
        sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES ('gateway_key', ?)")
            .bind(&key)
            .execute(&self.pool)
            .await?;
        Ok(key)
    }
}
