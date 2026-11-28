//! 统计按账户/别名筛选的契约：筛选维度来自 usage_logs 的归属三列，
//! 候选占比的分母是成功请求数，而不是含失败的总请求数。

use axum::body::Body;
use axum::http::{header, Method, Request};
use llmux_core::usage::UsageService;
use serde_json::{json, Value};

const FROM: i64 = 1_700_000_000_000;
const TO: i64 = 1_800_000_000_000;

async fn pool() -> sqlx::SqlitePool {
    llmux_server::test_state().await.pool
}

async fn insert(
    pool: &sqlx::SqlitePool,
    account_id: i64,
    model: &str,
    requested: Option<&str>,
    aggregate: Option<&str>,
    idx: Option<i64>,
    success: bool,
    input: i64,
) {
    sqlx::query(
        "INSERT INTO usage_logs
         (timestamp, account_id, provider_id, model, input_tokens, output_tokens,
          latency_ms, success, requested_model, aggregate_alias, candidate_idx)
         VALUES (?, ?, 'openai', ?, ?, 10, 100, ?, ?, ?, ?)",
    )
    .bind(FROM + 1)
    .bind(account_id)
    .bind(model)
    .bind(input)
    .bind(success as i64)
    .bind(requested)
    .bind(aggregate)
    .bind(idx)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn share_denominator_uses_success_count() {
    let pool = pool().await;
    insert(&pool, 1001, "m1", Some("X"), Some("G"), Some(0), true, 30).await;
    insert(&pool, 1001, "m1", Some("X"), Some("G"), Some(0), true, 20).await;
    insert(&pool, 1001, "m1", Some("X"), Some("G"), Some(0), false, 99).await;

    let rows = UsageService::new(pool)
        .get_breakdown_by_candidate(Some(FROM), Some(TO), None, None, "G", "aggregate")
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].account_id, 1001);
    assert_eq!(rows[0].candidate_idx, Some(0));
    assert_eq!(rows[0].requests, 3, "失败请求要出现在总量里");
    assert_eq!(rows[0].success_count, 2, "占比分母只能用成功数");
    assert_eq!(rows[0].input, 149);
}

#[tokio::test]
async fn alias_kind_ordinary_and_aggregate_are_distinct() {
    let pool = pool().await;
    // X→G：既属于「客户端点名 X」，也属于「G 实际承载」。
    insert(&pool, 1001, "m1", Some("X"), Some("G"), Some(0), true, 10).await;
    // 直连 X（不经过聚合）：只属于普通别名 X。
    insert(&pool, 1002, "m2", Some("X"), None, None, true, 20).await;
    // 别的客户端请求走 G：只属于聚合别名 G。
    insert(&pool, 1003, "m3", Some("Y"), Some("G"), Some(1), true, 30).await;

    let svc = UsageService::new(pool);
    let ordinary = svc
        .get_breakdown_by_candidate(Some(FROM), Some(TO), None, None, "X", "ordinary")
        .await
        .unwrap();
    let aggregate = svc
        .get_breakdown_by_candidate(Some(FROM), Some(TO), None, None, "G", "aggregate")
        .await
        .unwrap();

    assert_eq!(ordinary.len(), 2);
    assert_eq!(aggregate.len(), 2);
    assert!(ordinary.iter().all(|r| r.account_id != 1003));
    assert!(aggregate.iter().all(|r| r.account_id != 1002));
    assert!(ordinary.iter().any(|r| r.account_id == 1001));
    assert!(aggregate.iter().any(|r| r.account_id == 1001));
}

#[tokio::test]
async fn null_attribution_rows_are_grouped_without_panic() {
    let pool = pool().await;
    // 0039 之前的历史行，三列都是 NULL；按别名筛选时它们不应出现，
    // 但按 candidate_idx 分组本身要能安全处理 NULL。
    insert(&pool, 1004, "old", None, None, None, true, 10).await;
    insert(&pool, 1005, "g", Some("G"), Some("G"), None, true, 20).await;

    let rows = UsageService::new(pool)
        .get_breakdown_by_candidate(Some(FROM), Some(TO), None, None, "G", "aggregate")
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].candidate_idx, None);
    assert_eq!(rows[0].account_id, 1005);
}

#[tokio::test]
async fn stats_endpoint_applies_account_and_alias_filters() {
    let state = llmux_server::test_state().await;
    insert(&state.pool, 1001, "m1", Some("X"), Some("G"), Some(0), true, 10).await;
    insert(&state.pool, 1002, "m2", Some("Y"), Some("G"), Some(1), true, 20).await;
    let app = llmux_server::app(state);

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"username":"admin","password":"admin"}).to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    let cookie = resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|s| s.split(';').next())
        .find(|s| s.starts_with("llmux_session="))
        .unwrap()
        .to_string();

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!("/api/stats?start={FROM}&end={TO}&account_id=1001&alias=G&alias_kind=aggregate"))
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, 200, "body: {}", String::from_utf8_lossy(&body_bytes));
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(body["summary"]["total_requests"], json!(1), "body: {body}");
    assert_eq!(body["byCandidate"].as_array().unwrap().len(), 1);
    assert_eq!(body["byCandidate"][0]["accountId"], json!(1001));
}
