//! 模型列表里**纯重定向别名**不能产出幽灵模型。
//!
//! 背景（2026-11-19 生产实测）：`model_aliases` 有两类行 ——
//!   1. **绑定型**：`provider_id` 非空，或 `account_ids` 非空。它们描述「这个模型
//!      名该往哪个账户/上游打」，是真实的模型。
//!   2. **重定向型**：`provider_id` 与 `account_ids` 都为空，`target_model` 存的是
//!      *另一个别名的名字*（见 `llmux_core::dispatcher::redirect_target`）。它只是把
//!      请求改道，**本身不对应任何一个上游模型** —— 它指向的模型在别处已经列出来了。
//!
//! 模型列表的三处「merge alias custom models」只按 `target_model` 落一条件目，把
//! `provider_id` 为空的一律归到 `owned_by="custom"`。于是重定向别名会凭空造出一张
//! 挂着 `custom` 账户、模型 id 是另一个别名名的卡片 —— 模型管理页里那个「奇怪的
//! custom account」，点进去既没有账户也没有模型，全是这一类行（生产上是
//! `gpt-6-sol → gpt6sol`、`gpt-6-luna → os`）。同一个模型因此在 `[驭风-codex]
//! gpt6sol` 和 `[custom] gpt6sol` 下各出现一次。
//!
//! 判据必须是「**是否真的绑定**」而不是「`provider_id` 是否为空」：`provider_id`
//! 为空但 `account_ids` 非空的别名是**合法绑定**（按 id 绑定账户，UI 里选账户保存
//! 就是这个形状），把它们一起滤掉会让真实模型从列表里消失。

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use serde_json::{json, Value};

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

async fn get_available(app: &axum::Router, cookie: &str) -> Value {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/models/available")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// 预置一台账户 + 它的模型缓存（模型列表走 `load_persistent_snapshot`，没有缓存行
/// 就没有账户那一段，测试会假绿）。
///
/// `owned_by` 必须写进 `models_json` 本身：快照路径不重新打标，真实的写入方
/// （`persist_snapshot` / stream 的 account 事件）也是先按 owned_by 分组再存的。
async fn seed_account_with_cache(state: &llmux_server::app::AppState, id: i64, alias: &str, model: &str) {
    sqlx::query(
        "INSERT INTO accounts (id, alias, provider_id, api_key, is_active) VALUES (?, ?, 'custom', 'k', 1)",
    )
    .bind(id)
    .bind(alias)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO account_model_cache (account_id, alias, models_json, error, updated_at) \
         VALUES (?, ?, ?, NULL, ?)",
    )
    .bind(id)
    .bind(alias)
    .bind(json!([{"id": model, "object": "model", "created": 0, "owned_by": alias}]).to_string())
    .bind(1_700_000_000i64)
    .execute(&state.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn a_redirect_alias_does_not_invent_a_custom_owned_model() {
    let state = llmux_server::test_state().await;
    seed_account_with_cache(&state, 9001, "acct", "gpt6sol").await;

    // 绑定型别名：provider_id 非空 → 是真实模型，必须留着（这里指向的账户并不存在，
    // 因为「owned_by = provider」的条目只是历史行为；本测试只关心它没被误删）。
    sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, provider_id) VALUES ('bound', 'real-model', 'acct')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    // 纯重定向别名：provider_id 与 account_ids 都为空 → 不对应任何上游模型。
    sqlx::query(
        "INSERT INTO model_aliases (alias, target_model) VALUES ('gpt-6-sol', 'gpt6sol')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let app = llmux_server::app(state);
    let cookie = login(&app).await;
    let body = get_available(&app, &cookie).await;
    let models = body["data"].as_array().expect("data array");

    let custom_owned: Vec<&str> = models
        .iter()
        .filter(|m| m["owned_by"] == json!("custom"))
        .map(|m| m["id"].as_str().unwrap_or(""))
        .collect();
    assert!(
        custom_owned.is_empty(),
        "重定向别名不该造出 owned_by=custom 的幽灵模型，实际有: {custom_owned:?}\n{body}"
    );

    // 账户自己的模型照常出现。
    assert!(
        models.iter().any(|m| m["owned_by"] == json!("acct") && m["id"] == json!("gpt6sol")),
        "账户模型不该被误删: {body}"
    );
    // 绑定型别名照常出现。
    assert!(
        models.iter().any(|m| m["id"] == json!("real-model")),
        "绑定型别名不该被误删: {body}"
    );
}

/// `provider_id` 为空但 `account_ids` 非空是**合法绑定**（按 id 绑账户）。这类别名
/// 必须继续出现在列表里 —— 否则上面那条过滤会把真实模型一起吞掉。
#[tokio::test]
async fn an_alias_bound_by_account_ids_is_still_listed() {
    let state = llmux_server::test_state().await;
    seed_account_with_cache(&state, 9001, "acct", "seed-model").await;
    sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, account_ids) VALUES ('by-ids', 'ids-model', '[9001]')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let app = llmux_server::app(state);
    let cookie = login(&app).await;
    let body = get_available(&app, &cookie).await;
    let models = body["data"].as_array().expect("data array");
    assert!(
        models.iter().any(|m| m["id"] == json!("ids-model")),
        "按 account_ids 绑定的别名是真实模型，不能消失: {body}"
    );
}
