use llmux_core::adapters;
use std::sync::LazyLock;
use std::time::Instant;

static TIME_FMT_HELPERS: LazyLock<Vec<time::format_description::BorrowedFormatItem<'static>>> =
    LazyLock::new(|| time::format_description::parse_borrowed::<1>("[hour]:[minute]:[second]").unwrap());

use crate::app::TuiEvent;

/// Trim trailing slashes and prepend https:// when a scheme is missing —
/// reqwest fails with a Builder error on bare hostnames ("openrouter/api/v1").
pub fn normalize_base_url(value: &str) -> String {
    let t = value.trim().trim_end_matches('/');
    if t.contains("://") {
        t.to_string()
    } else {
        format!("https://{t}")
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
        let ts = time::OffsetDateTime::now_utc()
            .format(&TIME_FMT_HELPERS)
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

// Cap stored request/response bodies: requests get more room (Claude Code
// bodies carry big system prompts; the log detail tree shows them), responses
// stay compact. ~525 real requests/day → 32k adds ~17MB/day, ~500MB/month.
const REQUEST_BODY_CAP: usize = 32_000;
const RESPONSE_BODY_CAP: usize = 16_000;

// Bodies serve the recent log-detail view only; null them after 3 days so
// usage_logs growth stays bounded (rows/stats are kept).
const BODY_RETENTION_MS: i64 = 3 * 86_400_000;

async fn prune_old_bodies(pool: &sqlx::SqlitePool) {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
        - BODY_RETENTION_MS;
    if let Err(e) = sqlx::query(
        "UPDATE usage_logs SET request_body = NULL, response_body = NULL \
         WHERE timestamp < ? AND (request_body IS NOT NULL OR response_body IS NOT NULL)",
    )
    .bind(cutoff)
    .execute(pool)
    .await
    {
        tracing::debug!("📊 Failed to prune old bodies: {e}");
    }
}

fn truncate_body(s: Option<String>, cap: usize) -> Option<String> {
    s.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() { None } else {
            Some(if trimmed.chars().count() > cap {
                trimmed.chars().take(cap).collect()
            } else {
                trimmed.to_string()
            })
        }
    })
}

/// Client IP of the current request (set by RequestLogMiddleware's task-local).
/// Returns None outside a request context (background tasks, streams).
pub fn current_client_ip() -> Option<String> {
    crate::app::CLIENT_IP
        .try_with(|v| v.clone())
        .ok()
        .filter(|v| !v.is_empty())
}

// Fire-and-forget variant — does not block the response path.
// Reads the request-scoped client IP from the task-local automatically.
#[allow(clippy::too_many_arguments)]
pub fn spawn_log_usage(
    pool: sqlx::SqlitePool,
    account: adapters::Account,
    model: String,
    provider_id: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    latency_ms: i64,
    success: bool,
    error_message: Option<String>,
    request_body: Option<String>,
    response_body: Option<String>,
    ttft_ms: Option<i64>,
    is_stream: bool,
) {
    spawn_log_usage_ip(
        pool, account, model, provider_id, input_tokens, output_tokens,
        cache_read_input_tokens, cache_creation_input_tokens, latency_ms,
        success, error_message, request_body, response_body, ttft_ms, is_stream, current_client_ip(),
    );
}

// Same as spawn_log_usage but with an explicit client IP (needed by stream
// paths: tokio::spawn does not inherit the task-local).
#[allow(clippy::too_many_arguments)]
pub fn spawn_log_usage_ip(
    pool: sqlx::SqlitePool,
    account: adapters::Account,
    model: String,
    provider_id: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    latency_ms: i64,
    success: bool,
    error_message: Option<String>,
    request_body: Option<String>,
    response_body: Option<String>,
    ttft_ms: Option<i64>,
    is_stream: bool,
    client_ip: Option<String>,
) {
    let account = account.clone();
    let request_body = truncate_body(request_body, REQUEST_BODY_CAP);
    let response_body = truncate_body(response_body, RESPONSE_BODY_CAP);
    tokio::spawn(async move {
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        let res = sqlx::query("INSERT INTO usage_logs (timestamp, account_id, provider_id, model, input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, latency_ms, success, error_message, request_body, response_body, ttft_ms, is_stream, client_ip, is_test) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(timestamp).bind(account.id).bind(&provider_id).bind(&model)
            .bind(input_tokens).bind(output_tokens).bind(cache_read_input_tokens).bind(cache_creation_input_tokens)
            .bind(latency_ms).bind(if success {1} else {0}).bind(error_message.as_deref())
            .bind(request_body.as_deref()).bind(response_body.as_deref())
            .bind(if is_stream {1} else {0}).bind(ttft_ms).bind(client_ip.as_deref()).bind(0)
            .execute(&pool).await;
        if let Err(e) = res { tracing::error!("📊 Failed to insert usage log: {e}"); }
        prune_old_bodies(&pool).await;
    });
}

// ---------------------------------------------------------------------------
// Sync variant (used by background tasks)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
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
    request_body: Option<String>,
    response_body: Option<String>,
    ttft_ms: Option<i64>,
    is_stream: bool,
) -> anyhow::Result<()> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let request_body = truncate_body(request_body, REQUEST_BODY_CAP);
    let response_body = truncate_body(response_body, RESPONSE_BODY_CAP);

    let result = sqlx::query(
        "INSERT INTO usage_logs (
            timestamp, account_id, provider_id, model,
            input_tokens, output_tokens,
            cache_read_input_tokens, cache_creation_input_tokens,
            latency_ms, success, error_message, request_body, response_body,
            ttft_ms, is_stream, client_ip, is_test
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(request_body.as_deref())
    .bind(response_body.as_deref())
    .bind(ttft_ms)
    .bind(if is_stream { 1 } else { 0 })
    .bind(current_client_ip().as_deref())
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
