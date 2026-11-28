//! /api/stats/logs 的归属列契约：筛选与返回字段必须同源，
//! 否则日志页选了别名却仍能看到别的别名请求。

use axum::body::Body;
use axum::http::{header, Method, Request};
use llmux_core::usage::{DetailedLogQuery, UsageService};
use serde_json::{json, Value};

const FROM: i64 = 1_700_000_000_000;
const TO: i64 = 1_800_000_000_000;

async fn pool() -> sqlx::SqlitePool {
    llmux_server::test_state().await.pool
}

async fn insert(
    pool: &sqlx::SqlitePool,
    account_id: i64,
    requested: Option<&str>,
    aggregate: Option<&str>,
    idx: Option<i64>,
) {
    sqlx::query(
        "INSERT INTO usage_logs
         (timestamp, account_id, provider_id, model, input_tokens, output_tokens,
          latency_ms, success, requested_model, aggregate_alias, candidate_idx)
         VALUES (?, ?, 'openai', 'm', 10, 10, 100, 1, ?, ?, ?)",
    )
    .bind(FROM + 1)
    .bind(account_id)
    .bind(requested)
    .bind(aggregate)
    .bind(idx)
    .execute(pool)
    .await
    .unwrap();
}

fn query(account_id: Option<i64>, alias: Option<&str>, alias_kind: Option<&str>) -> DetailedLogQuery {
    DetailedLogQuery {
        start_time: Some(FROM),
        end_time: Some(TO),
        account_id,
        alias: alias.map(str::to_string),
        alias_kind: alias_kind.map(str::to_string),
        limit: Some(50),
        ..Default::default()
    }
}

#[tokio::test]
async fn detailed_logs_filter_by_account_and_alias_kind() {
    let pool = pool().await;
    insert(&pool, 1001, Some("X"), Some("G"), Some(0)).await;
    insert(&pool, 1002, Some("X"), None, None).await;
    insert(&pool, 1003, Some("Y"), Some("G"), Some(1)).await;
    let svc = UsageService::new(pool);

    let by_account = svc.get_detailed_logs(query(Some(1001), None, None)).await.unwrap();
    let by_ordinary = svc.get_detailed_logs(query(None, Some("X"), Some("ordinary"))).await.unwrap();
    let by_aggregate = svc.get_detailed_logs(query(None, Some("G"), Some("aggregate"))).await.unwrap();

    assert_eq!(by_account.len(), 1);
    assert_eq!(by_ordinary.len(), 2, "X→G 与直连 X 都算 X 的流量");
    assert_eq!(by_aggregate.len(), 2, "X→G 与 Y→G 都算 G 的流量");
    let count = svc.count_detailed_logs(query(None, Some("G"), Some("aggregate"))).await.unwrap();
    assert_eq!(count, 2, "分页 total 必须与筛选口径一致");
}

#[tokio::test]
async fn detailed_log_records_carry_route_attribution() {
    let pool = pool().await;
    insert(&pool, 1001, Some("X"), Some("G"), Some(2)).await;
    insert(&pool, 1002, None, None, None).await;
    let svc = UsageService::new(pool);

    let rows = svc.get_detailed_logs(query(None, None, None)).await.unwrap();
    assert_eq!(rows.len(), 2);
    let attributed = rows.iter().find(|r| r.account_id == Some(1001)).unwrap();
    assert_eq!(attributed.requested_model.as_deref(), Some("X"));
    assert_eq!(attributed.aggregate_alias.as_deref(), Some("G"));
    assert_eq!(attributed.candidate_idx, Some(2));
    let legacy = rows.iter().find(|r| r.account_id == Some(1002)).unwrap();
    assert_eq!(legacy.requested_model, None);
    assert_eq!(legacy.aggregate_alias, None);
    assert_eq!(legacy.candidate_idx, None);
}

#[tokio::test]
async fn logs_endpoint_maps_attribution_and_accepts_filters() {
    let state = llmux_server::test_state().await;
    insert(&state.pool, 1001, Some("X"), Some("G"), Some(2)).await;
    insert(&state.pool, 1002, Some("Y"), Some("G"), Some(1)).await;
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
        .uri(format!("/api/stats/logs?start={FROM}&end={TO}&alias=G&alias_kind=aggregate&account_id=1001"))
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, 200, "body: {}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["total"], json!(1), "body: {body}");
    assert_eq!(body["logs"][0]["requestedModel"], json!("X"));
    assert_eq!(body["logs"][0]["aggregateAlias"], json!("G"));
    assert_eq!(body["logs"][0]["candidateIdx"], json!(2));
}
