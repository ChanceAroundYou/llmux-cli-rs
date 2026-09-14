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
    models_inner(state, headers, false).await
}

/// Claude Desktop / Claude Code 的 gateway discovery 把 `/v1/models` 拼在它自己的
/// base URL 后面,并且客户端硬过滤掉 id 不匹配 `/(claude|anthropic)/i` 的条目。
/// Desktop 把 base URL 设成 `/{base}/cc`,所以只有这一个入口把别名广告成
/// `claude-<alias>`(display_name 仍是原名);其余入口的 `/v1/models` 保持裸别名。
/// 见 `crate::app::desktop_router` 与 `llmux_core::dispatcher::find_alias`。
pub async fn models_for_desktop(
    Extension(state): Extension<AppState>,
    headers: HeaderMap,
) -> Response {
    models_inner(state, headers, true).await
}

async fn models_inner(state: AppState, headers: HeaderMap, prefix_ids: bool) -> Response {
    // One line per discovery call: clients disagree about which headers they
    // send, so record what we actually saw when a picker shows no models.
    tracing::info!(
        "🤖 /v1/models ua={:?} anthropic_version={} x_api_key={}",
        headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-"),
        headers.contains_key("anthropic-version"),
        headers.contains_key("x-api-key"),
    );
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
    // context_length 反映聚合别名当前激活候选的模型，而非恒为第一个候选
    let active_map = state.aggregate_router.lock().unwrap().snapshot_actives();
    for (alias, candidates) in agg_rows {
        let target = llmux_core::aggregate::parse_candidates(&candidates)
            .ok()
            .and_then(|v| {
                if v.is_empty() {
                    return None;
                }
                let active = active_map.get(&alias).copied().unwrap_or(0).min(v.len() - 1);
                Some(v[active].model.clone())
            })
            .unwrap_or_default();
        alias_rows_with_agg.push((alias, Some(target)));
    }

    // Serve a single envelope that satisfies both the Anthropic and the OpenAI
    // model-list contracts. Request headers are not a reliable discriminator:
    // Claude Desktop's gateway mode sends `Authorization: Bearer` and may omit
    // `anthropic-version`, yet still parses the Anthropic envelope — header
    // sniffing handed it an OpenAI-shaped list whose items it silently dropped
    // ("Model discovery: found 0 models", gateway logs showing HTTP 200).
    // Both SDKs ignore unknown fields, so the superset is safe for each.
    let created_at = iso8601_now();
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let data: Vec<Value> = alias_rows_with_agg
        .iter()
        .map(|(alias, target)| {
            let id = if prefix_ids {
                advertised_id(alias)
            } else {
                alias.clone()
            };
            let mut obj = serde_json::json!({
                "type": "model",
                "id": id,
                "display_name": alias,
                "created_at": created_at,
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
    let first_id = data
        .first()
        .and_then(|m| m["id"].as_str().map(str::to_string));
    let last_id = data
        .last()
        .and_then(|m| m["id"].as_str().map(str::to_string));

    Json(serde_json::json!({
        "object": "list",
        "data": data,
        "has_more": false,
        "first_id": first_id,
        "last_id": last_id,
    }))
    .into_response()
}

/// Client model pickers drop every entry whose `id` fails `/(claude|anthropic)/i`
/// (hardcoded in Claude Desktop / Claude Code gateway discovery). Only the
/// Desktop mount (`/{base}/cc/v1/models`) pays that tax, publishing `claude-<alias>`
/// with the alias still in `display_name`; `llmux_core::dispatcher::find_alias`
/// maps the prefixed spelling back on the way in.
fn advertised_id(alias: &str) -> String {
    let lower = alias.to_ascii_lowercase();
    if lower.contains("claude") || lower.contains("anthropic") {
        alias.to_string()
    } else {
        format!("claude-{alias}")
    }
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
