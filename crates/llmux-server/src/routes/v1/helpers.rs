use llmux_core::adapters;
use std::time::Instant;

use crate::app::TuiEvent;

pub fn normalize_base_url(value: &str) -> String {
    value.trim_end_matches('/').to_string()
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
                &time::format_description::parse("[hour]:[minute]:[second]").unwrap(),
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
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ts = dur.as_secs() as i64;
    let ms = dur.subsec_millis();

    // Howard Hinnant's civil-from-days algorithm
    let days = ts / 86400;
    let sec_of_day = (ts % 86400) as u32;
    let h = sec_of_day / 3600;
    let m = (sec_of_day % 3600) / 60;
    let s = sec_of_day % 60;

    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 {
        (mp + 3) as u32
    } else {
        (mp - 9) as u32
    };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}.{ms:03}Z")
}

// ---------------------------------------------------------------------------
// Usage logging
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn log_usage(
    pool: &sqlx::SqlitePool,
    account: &adapters::Account,
    model: &str,
    provider_id: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    latency_ms: i64,
    success: bool,
    error_message: &Option<String>,
) -> anyhow::Result<()> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let result = sqlx::query(
        "INSERT INTO usage_logs (
            timestamp, account_id, provider_id, model,
            input_tokens, output_tokens,
            cache_read_input_tokens, cache_creation_input_tokens,
            latency_ms, success, error_message, is_test
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(timestamp)
    .bind(account.id)
    .bind(provider_id)
    .bind(model)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cache_read_input_tokens)
    .bind(cache_creation_input_tokens)
    .bind(latency_ms)
    .bind(if success { 1 } else { 0 })
    .bind(error_message.as_deref())
    .bind(0)
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
