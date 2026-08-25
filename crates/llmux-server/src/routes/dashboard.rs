use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;

/// Aggregated dashboard endpoint — single round-trip replaces the 5+2 fan-out
/// previously done from the UI (accounts / aliases / aggregate-aliases / health /
/// models/health + deferred activity/keys). Keeps legacy endpoints intact; this
/// is a pure additive read-only aggregation.
pub async fn get_dashboard(Extension(state): Extension<AppState>) -> Response {
    // Run all 7 reads concurrently. Each future maps its DB rows/errors into a
    // local Result; the outer join collects them so we can fail fast with 500.
    let (accounts_r, aliases_r, agg_r, health_r, model_health_r, activity_r, keys_r) = tokio::join!(
        fetch_accounts(&state),
        fetch_aliases(&state),
        fetch_aggregate_aliases(&state),
        fetch_health(&state),
        fetch_model_health(&state),
        fetch_activity(&state),
        fetch_keys_count(&state),
    );

    let accounts = match accounts_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("dashboard: accounts: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let aliases = match aliases_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("dashboard: aliases: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let aggregate_aliases = match agg_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("dashboard: aggregateAliases: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let health = match health_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("dashboard: health: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let model_health = match model_health_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("dashboard: modelHealth: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let activity = match activity_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("dashboard: activity: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let keys_count = match keys_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("dashboard: keys: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };

    Json(json!({
        "accounts": accounts,
        "aliases": aliases,
        "aggregateAliases": aggregate_aliases,
        "health": health,
        "modelHealth": model_health,
        "activity": activity,
        "keysCount": keys_count,
    }))
    .into_response()
}

async fn fetch_accounts(state: &AppState) -> anyhow::Result<Value> {
    // Mirrors accounts::list_accounts but without decrypting api_key — dashboard
    // only needs the count and alias list; avoid leaking keys in an aggregate.
    let rows = sqlx::query("SELECT id, alias, provider_id, base_url, anthropic_base_url, is_active, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol FROM accounts ORDER BY id DESC")
        .fetch_all(&state.pool)
        .await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<i64, _>("id").unwrap_or_default(),
                "alias": r.try_get::<String, _>("alias").unwrap_or_default(),
                "provider_id": r.try_get::<String, _>("provider_id").unwrap_or_default(),
                "base_url": r.try_get::<Option<String>, _>("base_url").unwrap_or_default(),
                "anthropic_base_url": r.try_get::<Option<String>, _>("anthropic_base_url").unwrap_or_default(),
                "is_active": r.try_get::<i64, _>("is_active").unwrap_or_default(),
                "chat_endpoint": r.try_get::<Option<String>, _>("chat_endpoint").unwrap_or_default(),
                "responses_endpoint": r.try_get::<Option<String>, _>("responses_endpoint").unwrap_or_default(),
                "messages_endpoint": r.try_get::<Option<String>, _>("messages_endpoint").unwrap_or_default(),
                "default_protocol": r.try_get::<Option<String>, _>("default_protocol").unwrap_or_default(),
            })
        })
        .collect();
    Ok(Value::Array(out))
}

async fn fetch_aliases(state: &AppState) -> anyhow::Result<Value> {
    let rows = sqlx::query_as::<_, llmux_core::models::ModelAlias>(
        "SELECT id, alias, target_model, provider_id, account_ids, preferred_account_id, upstream_api FROM model_aliases ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(serde_json::to_value(rows).unwrap_or(Value::Array(vec![])))
}

async fn fetch_aggregate_aliases(state: &AppState) -> anyhow::Result<Value> {
    let rows = sqlx::query_as::<_, llmux_core::aggregate::AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, upstream_api, created_at, updated_at FROM aggregate_aliases ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    let router = state.aggregate_router.lock().unwrap();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let candidates: Value = serde_json::from_str(&row.candidates).unwrap_or(Value::Array(vec![]));
        let entry = router.entries.get(&row.alias);
        let active = entry.map(|e| e.active).unwrap_or(0);
        let last_status = entry
            .map(|e| serde_json::to_value(&e.last_status).unwrap_or(Value::Array(vec![])))
            .unwrap_or(Value::Array(vec![]));
        let pending_target = entry.and_then(|e| e.pending_target.map(|v| json!(v))).unwrap_or(Value::Null);
        let confirm_count = entry.map(|e| json!(e.confirm_count)).unwrap_or(json!(0));
        out.push(json!({
            "id": row.id,
            "alias": row.alias,
            "candidates": candidates,
            "interval_secs": row.interval_secs.unwrap_or(300),
            "upstream_api": row.upstream_api.clone().unwrap_or_else(|| "chat".to_string()),
            "active": active,
            "last_status": last_status,
            "pending_target": pending_target,
            "confirm_count": confirm_count,
        }));
    }
    Ok(Value::Array(out))
}

async fn fetch_health(state: &AppState) -> anyhow::Result<Value> {
    // Reuse the optimized single-query health (mirrors health::get_health_status).
    let rows = sqlx::query(
        "SELECT a.id, a.alias, COALESCE(s.total, 0) AS total, COALESCE(s.success, 0) AS success \
         FROM accounts a LEFT JOIN (SELECT account_id, COUNT(*) AS total, SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS success FROM usage_logs GROUP BY account_id) s ON s.account_id = a.id ORDER BY a.id",
    )
    .fetch_all(&state.pool)
    .await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let id: i64 = r.try_get("id").unwrap_or_default();
            let alias: String = r.try_get("alias").unwrap_or_default();
            let total: i64 = r.try_get("total").unwrap_or_default();
            let success: i64 = r.try_get("success").unwrap_or_default();
            let status = if total > 0 {
                let rate = success as f64 / total as f64;
                if rate > 0.9 { "healthy" } else if rate > 0.5 { "degraded" } else { "down" }
            } else { "unknown" };
            json!({"id": format!("acc_{id}"), "name": alias, "status": status, "lastSuccess": success, "totalChecks": total})
        })
        .collect();
    Ok(Value::Array(out))
}

async fn fetch_model_health(state: &AppState) -> anyhow::Result<Value> {
    let rows = sqlx::query(
        "SELECT u.account_id, a.provider_id, u.model, u.timestamp AS last_checked, u.success, u.latency_ms AS latency, u.error_message AS error, a.limits_cache, a.limits_cache_updated_at, a.alias AS account_name \
         FROM usage_logs u JOIN accounts a ON u.account_id = a.id WHERE u.id IN (SELECT MAX(id) FROM usage_logs GROUP BY account_id, model)",
    )
    .fetch_all(&state.pool)
    .await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let limits_cache_str: Option<String> = r.try_get("limits_cache").unwrap_or_default();
            let limits_cache: Value = limits_cache_str.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
            json!({
                "account_id": r.try_get::<i64, _>("account_id").unwrap_or_default(),
                "provider_id": r.try_get::<String, _>("provider_id").unwrap_or_default(),
                "model": r.try_get::<String, _>("model").unwrap_or_default(),
                "last_checked": r.try_get::<i64, _>("last_checked").unwrap_or_default(),
                "success": r.try_get::<i64, _>("success").unwrap_or_default(),
                "latency": r.try_get::<i64, _>("latency").unwrap_or_default(),
                "error": r.try_get::<Option<String>, _>("error").unwrap_or_default(),
                "limits_cache": limits_cache,
                "limits_cache_updated_at": r.try_get::<Option<String>, _>("limits_cache_updated_at").unwrap_or_default(),
                "account_name": r.try_get::<String, _>("account_name").unwrap_or_default(),
            })
        })
        .collect();
    Ok(Value::Array(out))
}

async fn fetch_activity(state: &AppState) -> anyhow::Result<Value> {
    let rows = sqlx::query(
        "SELECT l.id, l.timestamp, l.model, l.success, l.latency_ms, l.error_message, l.input_tokens, l.output_tokens, l.cache_read_input_tokens, l.cache_creation_input_tokens, l.ttft_ms, l.is_stream, a.alias AS account_name, a.provider_id \
         FROM usage_logs l LEFT JOIN accounts a ON l.account_id = a.id WHERE l.is_test = 0 ORDER BY l.timestamp DESC LIMIT 100",
    )
    .fetch_all(&state.pool)
    .await?;
    let entries: Vec<Value> = rows
        .iter()
        .map(|r| {
            let cache = r.try_get::<i64, _>("cache_read_input_tokens").unwrap_or_default();
            json!({
                "id": r.try_get::<i64, _>("id").unwrap_or_default(),
                "timestamp": r.try_get::<i64, _>("timestamp").unwrap_or_default(),
                "model": r.try_get::<String, _>("model").unwrap_or_default(),
                "success": r.try_get::<i64, _>("success").unwrap_or_default(),
                "latency_ms": r.try_get::<i64, _>("latency_ms").unwrap_or_default(),
                "input_tokens": r.try_get::<i64, _>("input_tokens").unwrap_or_default(),
                "output_tokens": r.try_get::<i64, _>("output_tokens").unwrap_or_default(),
                "cache_tokens": cache,
                "ttft_ms": r.try_get::<Option<i64>, _>("ttft_ms").unwrap_or_default(),
                "is_stream": r.try_get::<i64, _>("is_stream").unwrap_or_default(),
                "error_message": r.try_get::<Option<String>, _>("error_message").unwrap_or_default(),
                "account_name": r.try_get::<String, _>("account_name").unwrap_or_default(),
                "provider_id": r.try_get::<String, _>("provider_id").unwrap_or_default(),
            })
        })
        .collect();
    let total = entries.len() as i64;
    let success = entries.iter().filter(|e| e["success"] == 1).count() as i64;
    Ok(json!({"entries": entries, "totalRequests": total, "successCount": success}))
}

async fn fetch_keys_count(state: &AppState) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT COUNT(*) AS cnt FROM api_keys").fetch_one(&state.pool).await?;
    Ok(row.try_get::<i64, _>("cnt").unwrap_or_default())
}
