use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;

pub async fn get_models_summary(Extension(state): Extension<AppState>) -> Response {
    let (aliases_r, agg_r, accounts_r, health_r, queue_r) = tokio::join!(
        fetch_aliases(&state),
        fetch_aggregate_aliases(&state),
        fetch_accounts_light(&state),
        fetch_health(&state),
        fetch_queue(&state),
    );

    let aliases = match aliases_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("summary: aliases: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let aggregate_aliases = match agg_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("summary: aggregate: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let accounts = match accounts_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("summary: accounts: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let health = match health_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("summary: health: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let queue = match queue_r {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("summary: queue: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };

    Json(json!({
        "aliases": aliases,
        "aggregateAliases": aggregate_aliases,
        "accounts": accounts,
        "health": health,
        "queue": queue,
    }))
    .into_response()
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

async fn fetch_accounts_light(state: &AppState) -> anyhow::Result<Value> {
    let rows = sqlx::query(
        "SELECT id, alias, provider_id, base_url, anthropic_base_url, is_active, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol FROM accounts ORDER BY id DESC",
    )
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

async fn fetch_health(state: &AppState) -> anyhow::Result<Value> {
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

async fn fetch_queue(state: &AppState) -> anyhow::Result<Value> {
    let q = state.test_queue.lock().unwrap();
    Ok(json!({
        "isRunning": q.is_running,
        "total": q.total,
        "current": q.current,
        "progress": q.progress,
    }))
}
