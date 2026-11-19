//! 模型缓存的刷新判据：**失败**的结果不能按成功 TTL 冻 24 小时。
//!
//! 背景（2026-11-19 生产实测）：`account_model_cache.updated_at` 在抓取失败时也写
//! 当前时间，于是 SSE / `stale` 标记只看时间戳时，「刚失败」和「刚抓成功」长得一模
//! 一样 —— Copilot 账户因为一次容器到 LAN 的网络故障失败，面板就把它的占位卡锁
//! 24 小时，只能靠手动刷新（`force=true`）才重试。
//!
//! 判据收在 `available::cache_needs_refresh`：`error` 非空 → 5 分钟短退避；
//! 否则 24h。三处消费（SSE 的 targets 过滤、SSE 的 `is_stale`、持久快照的 `stale`）
//! 必须同一口径。
//!
//! ⚠️ 还有一半不在判据里：`force` 路径（`do_fetch_models` + `persist_snapshot`）
//! 过去把 `error` 列一律写回 NULL，于是「失败」在库里被持久化成「刚刚成功」，
//! 短退避根本不会触发。`a_failed_force_refresh_records_the_error_so_it_can_be_retried`
//! 钉的就是这一半。

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use serde_json::{json, Value};
use sqlx::Row;

async fn login(app: &axum::Router) -> String {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"username": "admin", "password": "admin"}).to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    for val in resp.headers().get_all(header::SET_COOKIE) {
        if let Ok(s) = val.to_str() {
            if let Some(tok) = s.split(';').next() {
                if tok.starts_with("llmux_session=") {
                    return tok.to_string();
                }
            }
        }
    }
    panic!("no session cookie from /api/auth/login");
}

/// 建一台 active 的 custom 账户。`base_url` 指 `127.0.0.1:9` 时抓取会立刻
/// connection refused —— 不需要任何外呼就能拿到「失败」这条路径。
async fn create_account(app: &axum::Router, cookie: &str, alias: &str) -> i64 {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/accounts")
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "alias": alias,
                "provider_id": "custom",
                "api_key": "sk-mock",
                "base_url": "http://127.0.0.1:9/v1",
                "skip_validation": true,
            })
            .to_string(),
        ))
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    body["id"].as_i64().expect("account id")
}

async fn seed_cache(
    state: &llmux_server::app::AppState,
    account_id: i64,
    alias: &str,
    updated_at: i64,
    error: Option<&str>,
) {
    let models = if error.is_some() {
        json!([{
            "id": format!("{alias}-models-unavailable"),
            "name": alias,
            "object": "model",
            "created": 0,
            "owned_by": alias,
        }])
    } else {
        json!([{"id": "seed-model", "object": "model", "created": 0, "owned_by": alias}])
    };
    sqlx::query(
        "INSERT OR REPLACE INTO account_model_cache (account_id, alias, models_json, error, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(account_id)
    .bind(alias)
    .bind(models.to_string())
    .bind(error)
    .bind(updated_at)
    .execute(&state.pool)
    .await
    .unwrap();
}

/// 走 SSE 快照路径，把整个 body 读完（handler 在 `done` 后关闭 channel）。
async fn read_stream(app: &axum::Router, cookie: &str, account_id: i64) -> String {
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!("/api/models/available/stream?accounts={account_id}"))
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 同样是 1 小时前，**失败**过的账户必须重试 —— 判据不是时间戳单独决定的，
/// 而是「时间戳 + 上次是成功还是失败」。
#[tokio::test]
async fn a_one_hour_old_failure_is_retried() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;
    let id = create_account(&app, &cookie, "flaky").await;
    seed_cache(&state, id, "flaky", now_secs() - 3600, Some("Network error: boom")).await;

    let body = read_stream(&app, &cookie, id).await;
    assert!(
        body.contains("event: account"),
        "1 小时前失败过的账户必须被重新抓取，实际没有 account 事件:\n{body}"
    );
    assert!(
        !body.contains("\"skipped\":true"),
        "不该走「全部新鲜」的跳过分支:\n{body}"
    );
}

/// 刚刚才失败（30 秒前）不要立刻再打一遍上游 —— 否则每次开页面都重试，
/// 上游坏掉时反而变成持续压力。
#[tokio::test]
async fn a_just_failed_account_is_not_hammered() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;
    let id = create_account(&app, &cookie, "recent-fail").await;
    seed_cache(&state, id, "recent-fail", now_secs() - 30, Some("Network error: boom")).await;

    let body = read_stream(&app, &cookie, id).await;
    assert!(
        !body.contains("event: account"),
        "30 秒前的失败还在 5 分钟退避窗口内，不该重抓:\n{body}"
    );
}

/// 同样是 1 小时前，**成功**过的账户不该重抓（24h TTL）。与上面那条成对，
/// 一起钉死「失败与成功走不同 TTL」。
#[tokio::test]
async fn a_one_hour_old_success_is_not_refetched() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;
    let id = create_account(&app, &cookie, "stable").await;
    seed_cache(&state, id, "stable", now_secs() - 3600, None).await;

    let body = read_stream(&app, &cookie, id).await;
    assert!(
        !body.contains("event: account"),
        "1 小时前成功的账户不该被重抓（24h TTL）:\n{body}"
    );
    assert!(
        body.contains("\"skipped\":true"),
        "应走「全部新鲜」的跳过分支:\n{body}"
    );
}

/// `force` 路径必须把失败写进 `error` 列 —— 否则库里只留下「刚刚更新」的时间戳，
/// 短退避永远不会触发。
#[tokio::test]
async fn a_failed_force_refresh_records_the_error_so_it_can_be_retried() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;
    let id = create_account(&app, &cookie, "broken").await;

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/models/available?force=true")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let row = sqlx::query("SELECT error, updated_at FROM account_model_cache WHERE account_id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .expect("cache row must exist after force refresh");
    let error: Option<String> = row.try_get("error").unwrap();
    assert!(
        error.is_some(),
        "抓取失败必须写回 error 列，否则「失败」会被当成「刚刚成功」锁 24h"
    );
}
