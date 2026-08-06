use llmux_core::adapters;
use std::time::Instant;

use crate::app::TuiEvent;

pub fn normalize_base_url(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

/// OpenAI 兼容请求的上游端点（去尾部斜杠）。
/// gemini 协议 + openai_compatible 且未自定义 base_url → 厂商默认 base + /openai
/// （官方 OpenAI 兼容端点，如 https://generativelanguage.googleapis.com/v1beta/openai）。
pub fn effective_openai_base_url(account: &adapters::Account) -> String {
    if account.protocol == "gemini" && account.openai_compatible == 1 && !account.custom_base_url {
        let default_base = account
            .base_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .unwrap_or("https://generativelanguage.googleapis.com/v1beta");
        format!("{}/openai", default_base.trim_end_matches('/'))
    } else {
        account
            .base_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .unwrap_or("https://api.openai.com/v1")
            .trim_end_matches('/')
            .to_string()
    }
}

pub fn send_tui_request(
    tui_tx: &Option<tokio::sync::mpsc::UnboundedSender<TuiEvent>>,
    path: &str,
    status: u16,
    start: Instant,
    model: &str,
) {
    if let Some(tx) = tui_tx {
        let ts = time::OffsetDateTime::now_local()
            .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
            .format(
                &time::format_description::parse_borrowed::<2>("[hour]:[minute]:[second]").unwrap(),
            )
            .unwrap_or_default();
        let latency_ms = start.elapsed().as_millis() as i64;
        let _ = tx.send(TuiEvent::Request {
            timestamp: ts,
            method: "POST".to_string(),
            path: path.to_string(),
            status,
            latency_ms,
            model: model.to_string(),
        });
    }
}

// ---------------------------------------------------------------------------
// ISO 8601 timestamp (UTC, ms precision)
// ---------------------------------------------------------------------------

pub fn iso8601_now() -> String {
    use time::format_description::well_known::Rfc3339;
    // 用 time crate 替代手写公历算法（Howard Hinnant civil-from-days）
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Usage logging（最小化：无 token 列，account_name 写时快照）
// ---------------------------------------------------------------------------

pub async fn log_usage(
    pool: &sqlx::SqlitePool,
    account: &adapters::Account,
    model: &str,
    latency_ms: i64,
    success: bool,
    error_message: &Option<String>,
) -> anyhow::Result<()> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let result = sqlx::query(
        "INSERT INTO usage_logs (ts, account_id, account_name, model, latency_ms, success, error_message)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(ts)
    .bind(account.id)
    .bind(&account.name)
    .bind(model)
    .bind(latency_ms)
    .bind(if success { 1 } else { 0 })
    .bind(error_message.as_deref())
    .execute(pool)
    .await;

    match &result {
        Err(e) => {
            tracing::error!("📊 Failed to insert usage log: {e}");
            Err(anyhow::anyhow!("{e}"))
        }
        Ok(_) => Ok(()),
    }
}
