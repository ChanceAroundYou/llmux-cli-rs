use llmux_core::adapters;
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Instant;

use crate::app::TuiEvent;

static TIME_FMT_HELPERS: LazyLock<Vec<time::format_description::BorrowedFormatItem<'static>>> =
    LazyLock::new(|| time::format_description::parse_borrowed::<1>("[hour]:[minute]:[second]").unwrap());

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

// Cap stored request/response bodies: success stays compact (DB-friendly),
// failure gets 500k so a 350k hermes dump is fully queryable. Bodies are
// kept only 3 days — rows/stats remain.
const REQUEST_BODY_CAP_SUCCESS: usize = 32_000;
const REQUEST_BODY_CAP_FAILURE: usize = 500_000;
const RESPONSE_BODY_CAP_SUCCESS: usize = 16_000;
const RESPONSE_BODY_CAP_FAILURE: usize = 500_000;

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

fn truncate_field(s: &str, limit: usize) -> String {
    let count = s.chars().count();
    if count <= limit {
        return s.to_string();
    }
    let half = limit / 2;
    let head: String = s.chars().take(half).collect();
    let tail: String = s.chars().skip(count - half).collect();
    format!("{head}\n…[truncated {} chars]…\n{tail}", count - limit)
}

fn compress_messages(body: &mut Value, per_field_limit: usize) {
    let Some(msgs) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for m in msgs.iter_mut() {
        let Some(obj) = m.as_object_mut() else {
            continue;
        };
        if let Some(c) = obj.get_mut("content") {
            match c {
                Value::String(s) => {
                    let src = s.clone();
                    let t = truncate_field(&src, per_field_limit);
                    if t != src {
                        *s = t;
                    }
                }
                Value::Array(parts) => {
                    for p in parts.iter_mut() {
                        if let Some(t) = p.get("text").and_then(Value::as_str).map(|s| s.to_string()) {
                            let truncated = truncate_field(&t, per_field_limit);
                            if truncated != t {
                                p["text"] = Value::String(truncated);
                            }
                        }
                        // OpenAI content parts may also carry image_url etc — leave as-is
                    }
                }
                _ => {}
            }
        }
        if let Some(tcs) = obj.get_mut("tool_calls").and_then(Value::as_array_mut) {
            for tc in tcs.iter_mut() {
                let args_opt = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                if let Some(args) = args_opt {
                    let truncated = truncate_field(&args, per_field_limit);
                    if truncated != args {
                        tc["function"]["arguments"] = Value::String(truncated);
                    }
                }
            }
        }
        // tool result messages store content as string — already handled above
    }
}

fn smart_truncate_body(
    s: Option<String>,
    is_success: bool,
    cap_success: usize,
    cap_failure: usize,
) -> Option<String> {
    let s = s?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let cap = if is_success {
        cap_success
    } else {
        cap_failure
    };
    if trimmed.chars().count() <= cap {
        return Some(trimmed.to_string());
    }
    // 字段内“砍中间” — 逐级收紧直到 fits cap；比“raw 切 JSON”更保结构
    // （一次 raw 切会把 JSON 字符串切断，导致 control-char 解析失败）
    let mut per_field_limit = if is_success { 200 } else { 1000 };
    if let Ok(original) = serde_json::from_str::<Value>(trimmed) {
        if original.get("messages").and_then(Value::as_array).is_some() {
            let mut body = original.clone();
            loop {
                let mut candidate = body.clone();
                compress_messages(&mut candidate, per_field_limit);
                if let Ok(compressed) = serde_json::to_string(&candidate) {
                    if compressed.chars().count() <= cap {
                        return Some(compressed);
                    }
                    if per_field_limit <= 20 {
                        // 已压到 20 仍超 cap — 此时再做“头尾留 JSON”兜底
                        let count = compressed.chars().count();
                        let marker = format!(
                            "\n…[truncated {} chars: compressed still {} > cap {}]…\n",
                            count - cap,
                            count,
                            cap
                        );
                        let budget = cap.saturating_sub(marker.chars().count());
                        let half = budget / 2;
                        let head: String = compressed.chars().take(half).collect();
                        let tail: String = compressed.chars().skip(count - (budget - half)).collect();
                        return Some(format!("{head}{marker}{tail}"));
                    }
                }
                if per_field_limit <= 20 {
                    break;
                }
                per_field_limit = (per_field_limit / 2).max(20);
                body = original.clone();
            }
        }
    }
    let count = trimmed.chars().count();
    let marker = format!("\n…[truncated {} chars]…\n", count - cap);
    let budget = cap.saturating_sub(marker.chars().count());
    let half = budget / 2;
    let head: String = trimmed.chars().take(half).collect();
    let tail: String = trimmed.chars().skip(count - (budget - half)).collect();
    Some(format!("{head}{marker}{tail}"))
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
    if !success {
        if let Some(ref b) = request_body {
            let snippet: String = b.chars().take(2000).collect();
            tracing::warn!(
                "📦 failed request snippet model={} account={} body_len={} snippet={}",
                model,
                account.alias,
                b.len(),
                snippet.chars().take(2000).collect::<String>()
            );
        }
    }
    let request_body = smart_truncate_body(
        request_body,
        success,
        REQUEST_BODY_CAP_SUCCESS,
        REQUEST_BODY_CAP_FAILURE,
    );
    let response_body = smart_truncate_body(
        response_body,
        success,
        RESPONSE_BODY_CAP_SUCCESS,
        RESPONSE_BODY_CAP_FAILURE,
    );
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

    let request_body = smart_truncate_body(
        request_body,
        success,
        REQUEST_BODY_CAP_SUCCESS,
        REQUEST_BODY_CAP_FAILURE,
    );
    let response_body = smart_truncate_body(
        response_body,
        success,
        RESPONSE_BODY_CAP_SUCCESS,
        RESPONSE_BODY_CAP_FAILURE,
    );

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_long_body_compresses_fields_not_tail() {
        let long = "a".repeat(500);
        let mut msgs = Vec::new();
        for i in 0..121 {
            if i == 30 {
                msgs.push(serde_json::json!({"role":"assistant","content":"x","tool_calls":[]}));
            } else {
                msgs.push(serde_json::json!({"role":"assistant","content": long, "tool_calls":[{"id":"c","type":"function","function":{"name":"f","arguments": long}}]}));
            }
        }
        let body = serde_json::to_string(&serde_json::json!({"model":"od","messages": msgs})).unwrap();
        assert!(body.chars().count() > 32_000);
        let out = smart_truncate_body(Some(body), true, 32_000, 500_000).unwrap();
        assert!(out.chars().count() <= 32_000, "success should fit in 32k after compression");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["messages"][30]["tool_calls"], serde_json::json!([]), "empty tool_calls structure must be preserved");
    }

    #[test]
    fn failure_long_body_kept_up_to_500k() {
        let body = "x".repeat(400_000);
        let json_body = format!(r#"{{"model":"od","messages":[{{"role":"user","content":"{}"}}]}}"#, body);
        let out = smart_truncate_body(Some(json_body), false, 32_000, 500_000).unwrap();
        assert!(out.chars().count() <= 500_000);
        assert!(out.chars().count() > 32_000, "failure should not be capped at 32k");
    }
}
