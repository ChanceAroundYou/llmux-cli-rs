use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ActivityQuery {
    pub limit: Option<i64>,
}

/// Simple activity feed for the dashboard — recent requests without token details.
pub async fn get_activity(
    Extension(state): Extension<AppState>,
    Query(params): Query<ActivityQuery>,
) -> Response {
    let limit = params.limit.unwrap_or(50).min(200);

    let logs = match sqlx::query(
        "SELECT l.id, l.timestamp, l.model, l.success, l.latency_ms,
                l.error_message, l.input_tokens, l.output_tokens, l.cache_read_input_tokens, l.cache_creation_input_tokens, l.ttft_ms, l.is_stream,
                a.alias AS account_name, a.provider_id
         FROM usage_logs l
         LEFT JOIN accounts a ON l.account_id = a.id
         WHERE l.is_test = 0
         ORDER BY l.timestamp DESC
         LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to fetch activity: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let entries: Vec<Value> = logs
        .iter()
        .map(|row| {
            let cache = row.try_get::<i64, _>("cache_read_input_tokens").unwrap_or_default()
                + row.try_get::<i64, _>("cache_creation_input_tokens").unwrap_or_default();
            json!({
                "id": row.try_get::<i64, _>("id").unwrap_or_default(),
                "timestamp": row.try_get::<i64, _>("timestamp").unwrap_or_default(),
                "model": row.try_get::<String, _>("model").unwrap_or_default(),
                "success": row.try_get::<i64, _>("success").unwrap_or_default(),
                "latency_ms": row.try_get::<i64, _>("latency_ms").unwrap_or_default(),
                "input_tokens": row.try_get::<i64, _>("input_tokens").unwrap_or_default(),
                "output_tokens": row.try_get::<i64, _>("output_tokens").unwrap_or_default(),
                "cache_tokens": cache,
                "ttft_ms": row.try_get::<Option<i64>, _>("ttft_ms").unwrap_or_default(),
                "is_stream": row.try_get::<i64, _>("is_stream").unwrap_or_default(),
                "error_message": row.try_get::<Option<String>, _>("error_message").unwrap_or_default(),
                "account_name": row.try_get::<String, _>("account_name").unwrap_or_default(),
                "provider_id": row.try_get::<String, _>("provider_id").unwrap_or_default(),
            })
        })
        .collect();

    let total_requests: i64 = entries.len() as i64;
    let success_count: i64 = entries.iter().filter(|e| e["success"] == 1).count() as i64;

    Json(json!({
        "entries": entries,
        "totalRequests": total_requests,
        "successCount": success_count,
    }))
    .into_response()
}

/// Log detail: request/response bodies captured at dispatch time (nullable
/// for old rows or paths without a capture point).
///
/// 不过滤 `is_test` —— 请求日志页也会列拨测行，点进去得能看到详情。
/// 仪表盘的活动流（上面那个）仍然只取真实流量。
///
/// 不带 `part` 时返回完整的 request_body + response_body（既有行为，合约测试依赖）。
/// 带 `?part=request|response&offset=N` 时只返回该部分的**一个片段**：
/// body 放大后单条可达数 MB，一次全量返回会让详情弹窗首屏就卡住。
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ActivityDetailQuery {
    /// `request` | `response`；缺省表示旧的全量返回
    pub part: Option<String>,
    pub offset: Option<usize>,
    /// 片段大小，默认 32KB，上限 512KB
    pub limit: Option<usize>,
    /// 只要元信息（不带 body）。详情弹窗的 body 走分段接口，
    /// 这里若还带上全量 body 就等于白分段了。
    pub meta: Option<bool>,
}

pub async fn get_activity_detail(
    Extension(state): Extension<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Query(params): Query<ActivityDetailQuery>,
) -> Response {
    let row = match sqlx::query(
        "SELECT l.id, l.timestamp, l.model, l.success, l.latency_ms,
                l.error_message, l.input_tokens, l.output_tokens, l.cache_read_input_tokens, l.cache_creation_input_tokens, l.ttft_ms, l.is_stream,
                l.request_body, l.response_body, l.client_ip,
                a.alias AS account_name, a.provider_id
         FROM usage_logs l
         LEFT JOIN accounts a ON l.account_id = a.id
         WHERE l.id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return crate::error::simple_error("Activity not found", StatusCode::NOT_FOUND);
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to fetch activity detail: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let request_body = row.try_get::<Option<String>, _>("request_body").unwrap_or_default();
    let response_body = row.try_get::<Option<String>, _>("response_body").unwrap_or_default();

    // 分段模式：只回一个片段，外加 total/next_offset 供前端续拉。
    if let Some(part) = params.part.as_deref() {        let full = match part {
            "request" => request_body,
            "response" => response_body,
            other => {
                return crate::error::simple_error(
                    format!("unknown part: {other}"),
                    StatusCode::BAD_REQUEST,
                )
            }
        };
        let text = full.unwrap_or_default();
        // `offset` / `next_offset` 以**字节**计（对外可与 total 直接比较），
        // 但切分必须落在 char 边界，否则会切碎多字节字符、产出非法 JSON 片段。
        // `char_indices()` 的第 0 项就是 offset 自身，故 nth(limit) 恰好给出
        // 「再往后 limit 个字符」那个字符的起始字节。
        let offset = params.offset.unwrap_or(0).min(text.len());
        let limit = params.limit.unwrap_or(32 * 1024).clamp(1, 512 * 1024);
        let end = text[offset..]
            .char_indices()
            .nth(limit)
            .map(|(i, _)| offset + i)
            .unwrap_or(text.len());
        let chunk = text[offset..end].to_string();
        return Json(json!({
            "part": part,
            "offset": offset,
            "next_offset": if end < text.len() { Some(end) } else { None },
            "total": text.len(),
            "chunk": chunk,
            "eof": end >= text.len(),
        }))
        .into_response();
    }

    // ?meta=1：只要元信息，不带 body（body 走 ?part= 分段拉）。
    if params.meta.unwrap_or(false) {
        return Json(json!({
            "id": row.try_get::<i64, _>("id").unwrap_or_default(),
            "timestamp": row.try_get::<i64, _>("timestamp").unwrap_or_default(),
            "model": row.try_get::<String, _>("model").unwrap_or_default(),
            "success": row.try_get::<i64, _>("success").unwrap_or_default(),
            "latency_ms": row.try_get::<i64, _>("latency_ms").unwrap_or_default(),
            "input_tokens": row.try_get::<i64, _>("input_tokens").unwrap_or_default(),
            "output_tokens": row.try_get::<i64, _>("output_tokens").unwrap_or_default(),
            "cache_tokens": row.try_get::<i64, _>("cache_read_input_tokens").unwrap_or_default(),
            "ttft_ms": row.try_get::<Option<i64>, _>("ttft_ms").unwrap_or_default(),
            "is_stream": row.try_get::<i64, _>("is_stream").unwrap_or_default(),
            "error_message": row.try_get::<Option<String>, _>("error_message").unwrap_or_default(),
            "account_name": row.try_get::<String, _>("account_name").unwrap_or_default(),
            "provider_id": row.try_get::<String, _>("provider_id").unwrap_or_default(),
            "client_ip": row.try_get::<Option<String>, _>("client_ip").unwrap_or_default(),
        }))
        .into_response();
    }

    let cache_tokens = row.try_get::<i64, _>("cache_read_input_tokens").unwrap_or_default();
    Json(json!({
        "id": row.try_get::<i64, _>("id").unwrap_or_default(),
        "timestamp": row.try_get::<i64, _>("timestamp").unwrap_or_default(),
        "model": row.try_get::<String, _>("model").unwrap_or_default(),
        "success": row.try_get::<i64, _>("success").unwrap_or_default(),
        "latency_ms": row.try_get::<i64, _>("latency_ms").unwrap_or_default(),
        "input_tokens": row.try_get::<i64, _>("input_tokens").unwrap_or_default(),
        "output_tokens": row.try_get::<i64, _>("output_tokens").unwrap_or_default(),
        "cache_tokens": cache_tokens,
        "ttft_ms": row.try_get::<Option<i64>, _>("ttft_ms").unwrap_or_default(),
        "is_stream": row.try_get::<i64, _>("is_stream").unwrap_or_default(),
        "error_message": row.try_get::<Option<String>, _>("error_message").unwrap_or_default(),
        "account_name": row.try_get::<String, _>("account_name").unwrap_or_default(),
        "provider_id": row.try_get::<String, _>("provider_id").unwrap_or_default(),
        "request_body": request_body,
        "response_body": response_body,
        "client_ip": row.try_get::<Option<String>, _>("client_ip").unwrap_or_default(),
    }))
    .into_response()
}
