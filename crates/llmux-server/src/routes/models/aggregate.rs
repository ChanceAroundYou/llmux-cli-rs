use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use llmux_core::aggregate::{parse_candidates, AggregateAliasRow};

use crate::app::AppState;

pub async fn list_aggregate_aliases(Extension(state): Extension<AppState>) -> Response {
    let rows = match sqlx::query_as::<_, AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, upstream_api, created_at, updated_at FROM aggregate_aliases ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(v) => v,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to list aggregate aliases: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    };

    let router = state.aggregate_router.lock().unwrap();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let candidates: Value =
            serde_json::from_str(&row.candidates).unwrap_or(Value::Array(vec![]));
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
    Json(json!(out)).into_response()
}

pub async fn set_aggregate_alias(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(alias) = body
        .get("alias")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return crate::error::simple_error(
            "Missing required field: alias",
            StatusCode::BAD_REQUEST,
        );
    };

    let Some(candidates_val) = body.get("candidates") else {
        return crate::error::simple_error(
            "Missing required field: candidates",
            StatusCode::BAD_REQUEST,
        );
    };
    let arr = match candidates_val.as_array() {
        Some(a) => a,
        None => {
            return crate::error::simple_error(
                "candidates must be an array",
                StatusCode::BAD_REQUEST,
            )
        }
    };
    if arr.is_empty() {
        return crate::error::simple_error(
            "candidates must not be empty",
            StatusCode::BAD_REQUEST,
        );
    }

    let mut parsed = Vec::with_capacity(arr.len());
    let mut account_ids = Vec::with_capacity(arr.len());
    for item in arr {
        let account_id = match item.get("account_id").and_then(Value::as_i64) {
            Some(v) if v > 0 => v,
            _ => {
                return crate::error::simple_error(
                    "each candidate requires account_id (>0)",
                    StatusCode::BAD_REQUEST,
                )
            }
        };
        let model = match item.get("model").and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            Some(v) => v,
            None => {
                return crate::error::simple_error(
                    "each candidate requires non-empty model",
                    StatusCode::BAD_REQUEST,
                )
            }
        };
        parsed.push(json!({"account_id": account_id, "model": model}));
        account_ids.push(account_id);
    }

    let interval_secs = body
        .get("interval_secs")
        .and_then(Value::as_i64)
        .unwrap_or(300)
        .clamp(60, 3600);

    // Validate account_ids exist and are active (dedup: same account may appear with different models)
    {
        let mut unique_ids = account_ids.clone();
        unique_ids.sort_unstable();
        unique_ids.dedup();
        let placeholders: Vec<String> = unique_ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT id FROM accounts WHERE id IN ({}) AND is_active = 1",
            placeholders.join(",")
        );
        let mut query = sqlx::query(&sql);
        for id in &unique_ids {
            query = query.bind(id);
        }
        match query.fetch_all(&state.pool).await {
            Ok(rows) => {
                if rows.len() != unique_ids.len() {
                    return crate::error::simple_error(
                        "one or more account_id does not exist or is inactive",
                        StatusCode::BAD_REQUEST,
                    );
                }
            }
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to validate accounts: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            }
        }
    }

    let confirm = body.get("confirm").and_then(Value::as_bool).unwrap_or(false);
    let upstream_api = llmux_core::upstream_api::UpstreamApi::from_str(body.get("upstream_api").and_then(Value::as_str).unwrap_or("chat")).as_str().to_string();
    // Prevent alias collision with ordinary aliases unless explicitly confirmed
    let ordinary_exists: Option<i64> =
        match sqlx::query_scalar("SELECT id FROM model_aliases WHERE alias = ?")
            .bind(&alias)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to check alias collision: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            }
        };
    if ordinary_exists.is_some() {
        if !confirm {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!("alias '{alias}' already exists as a regular alias"),
                    "code": "alias_conflict",
                    "conflict": "ordinary",
                    "alias": alias,
                })),
            )
                .into_response();
        }
        // confirmed — delete the ordinary alias (keep api_keys allowed_models entry, alias name survives as aggregate)
        let _ = sqlx::query("DELETE FROM model_aliases WHERE alias = ?")
            .bind(&alias)
            .execute(&state.pool)
            .await;
        state.invalidate_model_cache(&alias);
        if let Ok(mut cache) = state.models_cache.lock() {
            *cache = None;
        }
        state.clear_auth_cache();
        tracing::info!("🔀 Overwrote ordinary alias '{}' with aggregate alias (confirmed)", alias);
    }

    // Validate candidates JSON is parseable via shared parser
    let candidates_json = serde_json::to_string(&parsed).unwrap_or_else(|_| "[]".to_string());
    if let Err(e) = parse_candidates(&candidates_json) {
        return crate::error::simple_error(
            format!("Invalid candidates: {e}"),
            StatusCode::BAD_REQUEST,
        );
    }

    match sqlx::query(
        "INSERT INTO aggregate_aliases (alias, candidates, interval_secs, upstream_api, updated_at) VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP) ON CONFLICT(alias) DO UPDATE SET candidates=excluded.candidates, interval_secs=excluded.interval_secs, upstream_api=excluded.upstream_api, updated_at=CURRENT_TIMESTAMP",
    )
    .bind(&alias)
    .bind(&candidates_json)
    .bind(interval_secs)
    .bind(&upstream_api)
    .execute(&state.pool)
    .await
    {
        Ok(_) => {
            state.aggregate_router.lock().unwrap().remove(&alias);
            state.invalidate_aggregate_cache(&alias);
            if let Ok(mut cache) = state.models_cache.lock() {
                *cache = None;
            }
            tracing::info!("🔀 Set aggregate alias {} ({} candidates)", alias, parsed.len());
            Json(json!({ "success": true, "message": "Aggregate alias set successfully" })).into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to set aggregate alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn delete_aggregate_alias(
    Extension(state): Extension<AppState>,
    Path(id): Path<String>,
) -> Response {
    let row = match sqlx::query_as::<_, AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, upstream_api, created_at, updated_at FROM aggregate_aliases WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            return crate::error::simple_error("Aggregate alias not found", StatusCode::NOT_FOUND)
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to lookup aggregate alias: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    };

    if let Err(e) = sqlx::query("DELETE FROM aggregate_aliases WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
    {
        return crate::error::simple_error(
            format!("Failed to delete aggregate alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // Cascade-clean api_keys allowed_models (same as ordinary alias)
    if let Ok(api_keys) = sqlx::query_as::<_, (i64, String)>("SELECT id, allowed_models FROM api_keys")
        .fetch_all(&state.pool)
        .await
    {
        for (key_id, allowed_models) in &api_keys {
            if allowed_models == "*" {
                continue;
            }
            if let Ok(mut models) = serde_json::from_str::<Vec<String>>(allowed_models) {
                if models.contains(&row.alias) {
                    models.retain(|m| m != &row.alias);
                    let updated = if models.is_empty() {
                        "*".to_string()
                    } else {
                        serde_json::to_string(&models).unwrap_or_else(|_| "*".to_string())
                    };
                    let _ = sqlx::query("UPDATE api_keys SET allowed_models = ? WHERE id = ?")
                        .bind(&updated)
                        .bind(key_id)
                        .execute(&state.pool)
                        .await;
                }
            }
        }
    }

    state.aggregate_router.lock().unwrap().remove(&row.alias);
    state.invalidate_aggregate_cache(&row.alias);
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }
    state.clear_auth_cache();

    Json(json!({ "success": true, "message": "Aggregate alias deleted" })).into_response()
}

pub async fn set_aggregate_active(
    Extension(state): Extension<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let row = match sqlx::query_as::<_, AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, upstream_api, created_at, updated_at FROM aggregate_aliases WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return crate::error::simple_error("Aggregate alias not found", StatusCode::NOT_FOUND),
        Err(e) => return crate::error::simple_error(format!("Failed to lookup aggregate alias: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let candidates = match serde_json::from_str::<Vec<Value>>(&row.candidates) {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("Invalid candidates: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let len = candidates.len();
    if len == 0 {
        return crate::error::simple_error("Aggregate alias has no candidates", StatusCode::BAD_REQUEST);
    }
    let Some(active) = body.get("active").and_then(Value::as_i64) else {
        return crate::error::simple_error("Missing required field: active", StatusCode::BAD_REQUEST);
    };
    if active < 0 || (active as usize) >= len {
        return crate::error::simple_error(format!("active must be 0..{}", len - 1), StatusCode::BAD_REQUEST);
    }
    let target = active as usize;
    let alias = row.alias.clone();
    let changed = state.aggregate_router.lock().unwrap().set_active(&alias, target, len);
    state.invalidate_aggregate_cache(&alias);
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }
    tracing::info!("🔀 [agg:{}] active switched -> {} (manual)", alias, target);
    let _ = changed;
    Json(json!({ "success": true, "active": target })).into_response()
}
