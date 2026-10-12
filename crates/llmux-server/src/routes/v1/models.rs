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
    // 手动禁用的 (account_id, model) 对集合。每个别名绑定哪台账户是已知的
    // （account_ids JSON / provider_id 别名），禁掉了就别再广告出去 —— 客户端
    // 会把它列进模型选择器，选中必然 404，不如一开始就不露面。
    let disabled: std::collections::HashSet<(i64, String)> = {
        let rows: Vec<(i64, String)> =
            sqlx::query_as("SELECT account_id, model FROM disabled_models")
                .fetch_all(&state.pool)
                .await
                .unwrap_or_default();
        rows.into_iter().collect()
    };
    let account_alias_ids: Vec<(i64, String)> = sqlx::query_as("SELECT id, alias FROM accounts")
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    let alias_account_ids: std::collections::HashMap<String, i64> = account_alias_ids
        .iter()
        .map(|(id, a)| (a.clone(), *id))
        .collect();
    let mut alias_rows_with_agg = alias_rows;    // `context_length` 报**配置的主候选**（candidates[0]）的窗口，而不是运行时
    // 临时迁移到的那个。
    //
    // 为什么不用 `active_map`（曾经如此）：激活下标是**健康度驱动的临时切换**。
    // 大请求让主候选吃了 3 次「装不下」就迁移，别名随即改口报兜底模型的更大窗口
    // —— 客户端据此以为还有余量、于是不压缩，大请求继续来，主候选永远回不来。
    // 实测 2026-10-05：`os` 在 V=1 时报 1000000（deepseek），而它的主力
    // `ling-3.1-flash:free` 只有 262144，自锁成环。
    //
    // 报配置的那一个才是稳定的：窗口是「这个别名被设计成多大」，不该随一次
    // 临时故障转移而跳变。消费方（hermes 等按 OpenRouter 约定读该字段的客户端）
    // 据此压缩，压到主候选装得下的尺寸，主候选就能持续服务。
    for (alias, candidates) in agg_rows {
        let target = llmux_core::aggregate::parse_candidates(&candidates)
            .ok()
            .and_then(|v| v.first().map(|c| c.model.clone()))
            .unwrap_or_default();
        alias_rows_with_agg.push((alias, Some(target)));
    }

    // 原生别名（上面只取了 alias + target_model）再补一份 account_ids JSON
    let alias_account_ids_map: std::collections::HashMap<String, Vec<i64>> =
        sqlx::query_as::<_, (String, Option<String>)>("SELECT alias, account_ids FROM model_aliases")
            .fetch_all(&state.pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(a, ids)| {
                let parsed: Vec<i64> = ids
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                (a, parsed)
            })
            .collect();

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
        .filter(|(alias, target)| {
            let ids = alias_account_ids_map.get(alias.as_str());
            let target = target.as_deref().unwrap_or("");
            if target.is_empty() {
                return true;
            }
            let Some(ids) = ids else { return true };
            if ids.is_empty() {
                // provider_id 别名对准的账户只有一个
                let Some(a) = alias_account_ids.get(alias.as_str()) else { return true };
                return !disabled.contains(&(*a, target.to_string()));
            }
            !ids.iter().any(|id| disabled.contains(&(*id, target.to_string())))
        })
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
