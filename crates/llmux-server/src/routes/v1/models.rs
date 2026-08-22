use axum::{
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::Value;

use llmux_core::context::lookup_context_length;

use crate::app::AppState;
use crate::middleware;

use super::helpers::iso8601_now;

// ---------------------------------------------------------------------------
// /v1/models
// ---------------------------------------------------------------------------

pub async fn models(Extension(state): Extension<AppState>, headers: HeaderMap) -> Response {
    tracing::info!("🤖 Request received");
    let is_anthropic =
        headers.contains_key("x-api-key") || headers.contains_key("anthropic-version");

    let alias_rows: Vec<(String, Option<String>)> = match sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT alias, target_model FROM model_aliases",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return middleware::send_error(
                &format!("Failed to load models: {e}"),
                "server_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                is_anthropic,
            );
        }
    };
    // Merge aggregate aliases as first-class models
    let agg_rows: Vec<(String, String)> =
        match sqlx::query_as::<_, (String, String)>("SELECT alias, candidates FROM aggregate_aliases")
            .fetch_all(&state.pool)
            .await
        {
            Ok(rows) => rows,
            Err(_) => Vec::new(),
        };
    let mut alias_rows_with_agg = alias_rows;
    for (alias, candidates) in agg_rows {
        let target = llmux_core::aggregate::parse_candidates(&candidates)
            .ok()
            .and_then(|v| v.into_iter().next().map(|c| c.model))
            .unwrap_or_default();
        alias_rows_with_agg.push((alias, Some(target)));
    }

    if is_anthropic {
        let created_at = iso8601_now();
        let data: Vec<Value> = alias_rows_with_agg
            .iter()
            .map(|(alias, target)| {
                let mut obj = serde_json::json!({
                    "type": "model",
                    "id": alias,
                    "display_name": alias,
                    "created_at": created_at,
                });
                if let Some(ctx) = resolve_alias_context(&state, target.as_deref()) {
                    obj["context_length"] = serde_json::json!(ctx);
                }
                obj
            })
            .collect();
        let first_id = data
            .first()
            .and_then(|m| m["id"].as_str().map(str::to_string));
        let last_id = data
            .last()
            .and_then(|m| m["id"].as_str().map(str::to_string));
        return Json(serde_json::json!({
            "data": data,
            "has_more": false,
            "first_id": first_id,
            "last_id": last_id,
        }))
        .into_response();
    }

    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let data: Vec<Value> = alias_rows_with_agg
        .into_iter()
        .map(|(alias, target)| {
            let mut obj = serde_json::json!({
                "id": alias,
                "object": "model",
                "created": created,
                "owned_by": "llmux",
            });
            if let Some(ctx) = resolve_alias_context(&state, target.as_deref()) {
                obj["context_length"] = serde_json::json!(ctx);
            }
            obj
        })
        .collect();

    Json(serde_json::json!({
        "object": "list",
        "data": data
    }))
    .into_response()
}

/// Resolve an alias's context length: match its target model against the
/// cached upstream model list first, then fall back to the built-in table.
/// Takes the larger of upstream vs table to survive stale upstream data
/// (e.g. Agnes reports 200k while the real window is 512k/1M).
fn resolve_alias_context(state: &AppState, target_model: Option<&str>) -> Option<u64> {
    let target = target_model?;
    if target.is_empty() {
        return None;
    }
    let mut upstream: Option<u64> = None;
    if let Some(cache) = state.models_cache.lock().unwrap().as_ref() {
        for m in &cache.data {
            if m.get("id").and_then(Value::as_str) == Some(target) {
                if let Some(ctx) = m.get("context_length").and_then(Value::as_u64) {
                    upstream = Some(ctx);
                    break;
                }
            }
        }
    }
    let table = lookup_context_length(target);
    match (upstream, table) {
        (Some(u), Some(t)) => Some(u.max(t)),
        (Some(u), None) => Some(u),
        (None, Some(t)) => Some(t),
        (None, None) => None,
    }
}
