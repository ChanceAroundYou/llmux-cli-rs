use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    Extension,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use llmux_core::dispatcher::{get_active_accounts, resolve_provider_type};

use crate::app::AppState;
use super::available::fetch_provider_models;

/// SSE stream: snapshot first, then one `account` event per upstream account.
pub async fn stream_available_models(
    Extension(state): Extension<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let force = params.get("force").map(|v| v == "true").unwrap_or(false);
    // optional ?accounts=1,2,3 filter
    let filter_ids: Option<std::collections::HashSet<i64>> = params.get("accounts").and_then(|s| {
        if s.trim().is_empty() { return None; }
        let set: std::collections::HashSet<i64> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if set.is_empty() { None } else { Some(set) }
    });

    let (tx, rx) = mpsc::channel::<Result<String, std::convert::Infallible>>(32);
    let state_task = state.clone();

    tokio::spawn(async move {
        // 1) snapshot from DB (fast)
        let (snapshot_data, per_account) = load_snapshot(&state_task.pool).await;
        let cached_at = per_account.iter().map(|p| p["updated_at"].as_i64().unwrap_or(0)).max().unwrap_or(0);
        let snap_payload = json!({ "data": snapshot_data, "per_account": per_account, "cached_at": cached_at, "stale": is_stale(&per_account) });
        let _ = tx.send(Ok(format!("event: snapshot\ndata: {}\n\n", snap_payload))).await;

        // 2) resolve accounts to refresh
        let accounts = match get_active_accounts(&state_task.pool, None, &state_task.master_key).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("stream: list accounts failed: {e}");
                let _ = tx.send(Ok(format!("event: error\ndata: {}\n\n", json!({"error": e.to_string()})))).await;
                let _ = tx.send(Ok("event: done\ndata: {\"total\":0}\n\n".to_string())).await;
                return;
            }
        };
        let mut targets: Vec<_> = accounts.into_iter().filter(|a| {
            if let Some(ref set) = filter_ids { set.contains(&(a.id as i64)) } else { true }
        }).collect();

        if !force {
            // only refresh stale/missing entries (TTL 24h)
            const TTL: i64 = 24 * 60 * 60;
            let now = now_secs();
            // build map of cached updated_at
            let cached_map: std::collections::HashMap<i64, i64> = {
                let rows = sqlx::query("SELECT account_id, updated_at FROM account_model_cache")
                    .fetch_all(&state_task.pool).await.unwrap_or_default();
                rows.into_iter().map(|r| {
                    let id: i64 = r.try_get("account_id").unwrap_or(0);
                    let ts: i64 = r.try_get("updated_at").unwrap_or(0);
                    (id, ts)
                }).collect()
            };
            targets.retain(|a| {
                let ts = cached_map.get(&(a.id as i64)).copied().unwrap_or(0);
                now - ts >= TTL
            });
            if targets.is_empty() {
                let _ = tx.send(Ok(format!("event: done\ndata: {}\n\n", json!({"total": 0, "skipped": true})))).await;
                return;
            }
        }

        // 3) fetch per account with limited concurrency (4)
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
        let mut pending: futures_util::stream::FuturesUnordered<_> = futures_util::stream::FuturesUnordered::new();
        for account in targets {
            let state_c = state_task.clone();
            let tx_c = tx.clone();
            let sem_c = sem.clone();
            pending.push(async move {
                let _permit = sem_c.acquire().await.unwrap();
                let provider_type = resolve_provider_type(None, &account.provider_id);
                let (mut models, fetch_error) = fetch_provider_models(&account, &provider_type).await;
                let now = now_secs();
                // ensure owned_by and handle empty -> placeholder
                let payload_models: Vec<Value>;
                if models.is_empty() {
                    let mut ph = json!({
                        "id": format!("{}-models-unavailable", account.alias),
                        "name": account.alias,
                        "object": "model",
                        "created": 0,
                        "owned_by": account.alias,
                    });
                    if let Some(ref e) = fetch_error { ph["error"] = json!(e); }
                    payload_models = vec![ph.clone()];
                    // persist placeholder as single-item array so snapshot can reconstruct
                    let j = serde_json::to_string(&payload_models).unwrap_or_else(|_| "[]".into());
                    let _ = sqlx::query("INSERT OR REPLACE INTO account_model_cache (account_id, alias, models_json, error, updated_at) VALUES (?, ?, ?, ?, ?)")
                        .bind(account.id as i64).bind(&account.alias).bind(&j).bind(&fetch_error).bind(now)
                        .execute(&state_c.pool).await;
                } else {
                    for m in &mut models {
                        if let Value::Object(obj) = m { obj.insert("owned_by".to_string(), json!(account.alias)); }
                    }
                    payload_models = models.clone();
                    let j = serde_json::to_string(&payload_models).unwrap_or_else(|_| "[]".into());
                    let _ = sqlx::query("INSERT OR REPLACE INTO account_model_cache (account_id, alias, models_json, error, updated_at) VALUES (?, ?, ?, ?, ?)")
                        .bind(account.id as i64).bind(&account.alias).bind(&j).bind(&fetch_error).bind(now)
                        .execute(&state_c.pool).await;
                }
                // also refresh in-memory cache for v1/models compatibility (best-effort)
                {
                    let snapshot = load_snapshot(&state_c.pool).await.0;
                    if let Ok(mut c) = state_c.models_cache.lock() {
                        let now2 = now_secs();
                        *c = Some(crate::app::ModelsCache { data: snapshot, created_at: now2, refreshing: false });
                    }
                }
                let frame = json!({
                    "account_id": account.id,
                    "alias": account.alias,
                    "models": payload_models,
                    "error": fetch_error,
                    "updated_at": now,
                });
                let _ = tx_c.send(Ok(format!("event: account\ndata: {}\n\n", frame))).await;
            });
        }
        while pending.next().await.is_some() {}
        let _ = tx.send(Ok("event: done\ndata: {\"total\":1}\n\n".to_string())).await;
    });

    let stream = ReceiverStream::new(rx);
    let body = Body::from_stream(stream);
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("text/event-stream"));
    headers.insert("cache-control", HeaderValue::from_static("no-cache"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    (headers, body).into_response()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

async fn load_snapshot(pool: &sqlx::SqlitePool) -> (Vec<Value>, Vec<Value>) {
    let rows = sqlx::query("SELECT c.account_id, c.alias, c.models_json, c.error, c.updated_at FROM account_model_cache c JOIN accounts a ON a.id = c.account_id AND a.is_active = 1 ORDER BY c.updated_at DESC")
        .fetch_all(pool).await.unwrap_or_default();
    let mut all: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut per_account: Vec<Value> = Vec::new();
    for r in &rows {
        let account_id: i64 = r.try_get("account_id").unwrap_or(0);
        let alias: String = r.try_get("alias").unwrap_or_default();
        let j: String = r.try_get("models_json").unwrap_or_else(|_| "[]".into());
        let err: Option<String> = r.try_get("error").unwrap_or(None);
        let updated_at: i64 = r.try_get("updated_at").unwrap_or(0);
        let models: Vec<Value> = serde_json::from_str(&j).unwrap_or_default();
        for m in &models {
            let id = m.get("id").and_then(Value::as_str).unwrap_or("");
            let key = format!("{}:{}", alias, id);
            if seen.insert(key) { all.push(m.clone()); }
        }
        per_account.push(json!({"account_id": account_id, "alias": alias, "updated_at": updated_at, "error": err, "count": models.len()}));
    }
    // merge custom alias models (not tied to a cache row)
    let alias_rows = sqlx::query("SELECT DISTINCT target_model, provider_id FROM model_aliases WHERE target_model IS NOT NULL AND target_model != ''")
        .fetch_all(pool).await.unwrap_or_default();
    for r in alias_rows {
        let model_id: String = r.try_get("target_model").unwrap_or_default();
        let provider: String = r.try_get("provider_id").unwrap_or_default();
        if model_id.is_empty() { continue; }
        let owned_by = if provider.is_empty() { "custom".to_string() } else { provider.clone() };
        let key = format!("{}:{}", owned_by, model_id);
        if seen.insert(key) {
            all.push(json!({"id": model_id, "object": "model", "created": 0, "owned_by": owned_by}));
        }
    }
    (all, per_account)
}

fn is_stale(per_account: &[Value]) -> bool {
    if per_account.is_empty() { return true; }
    const TTL: i64 = 24*60*60;
    let now = now_secs();
    per_account.iter().any(|p| {
        let ts = p.get("updated_at").and_then(Value::as_i64).unwrap_or(0);
        now - ts >= TTL
    })
}
