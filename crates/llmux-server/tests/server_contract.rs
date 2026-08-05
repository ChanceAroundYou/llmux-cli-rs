use axum::body::Body;
use http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};

async fn request_json(method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state);
    let mut builder = Request::builder().method(method).uri(path);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(match body {
            Some(value) => Body::from(value.to_string()),
            None => Body::empty(),
        })
        .unwrap();

    let response = llmux_server::test_request(app, request).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// 共享同一 app/state 的请求助手（多步流程用，如 建账户 → 绑 alias）。
async fn request_json_shared(
    app: &axum::Router,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(match body {
            Some(value) => Body::from(value.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let response = llmux_server::test_request(app.clone(), request).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn request_text(method: Method, path: &str) -> (StatusCode, String, Option<String>) {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state);
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let response = llmux_server::test_request(app, request).await;
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    (status, text, content_type)
}

#[tokio::test]
async fn api_read_routes_match_gateway_empty_placeholder_shapes() {
    let cases = [
        ("/api/health", json!([])),
        ("/api/settings", json!({})),
        (
            "/api/export",
            json!({"version": 2, "accounts": [], "aliases": [], "keys": [], "settings": []}),
        ),
        ("/api/accounts", json!([])),
        ("/api/keys", json!([])),
        // /api/models/available returns { data, stale, cached_at } — check shape below
        ("/api/models/available", json!({"data": []})),
        ("/api/models/aliases", json!([])),
        ("/api/models/health", json!([])),
        (
            "/api/models/test-queue/status",
            json!({"isRunning": false, "total": 0, "current": 0, "progress": 0}),
        ),
        (
            "/api/activity",
            json!({"entries": [], "totalRequests": 0, "successCount": 0}),
        ),
        // /api/system/tools and claude-settings tested separately - depend on host state
        ("/api/system/claude-backups", json!([])),
    ];

    for (path, expected) in cases {
        let (status, body) = request_json(Method::GET, path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        if path == "/api/models/available" {
            // Response is { data, stale, cached_at }
            assert!(body["data"].is_array(), "{path}: data must be array");
            assert!(body["stale"].is_boolean(), "{path}: stale must be bool");
        } else {
            assert_eq!(body, expected, "{path}");
        }
    }
}

#[tokio::test]
async fn vendors_route_returns_seeded_catalog() {
    let (status, body) = request_json(Method::GET, "/api/vendors", None).await;
    assert_eq!(status, StatusCode::OK);
    let vendors = body.as_array().expect("vendors must be an array");
    assert!(vendors.len() >= 9, "expected seed vendors, got {}", vendors.len());
    let ids: Vec<&str> = vendors
        .iter()
        .filter_map(|v| v["id"].as_str())
        .collect();
    assert!(ids.contains(&"openai"));
    assert!(ids.contains(&"anthropic"));
    assert!(ids.contains(&"gemini"));
    // 内置厂商标记
    assert_eq!(vendors[0]["builtin"], json!(1));
}

#[tokio::test]
async fn api_write_routes_validate_required_fields_and_return_gateway_shapes() {
    let (status, body) = request_json(Method::POST, "/api/auth/web-session", Some(json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "Missing token or provider"}));

    let (status, body) = request_json(Method::POST, "/api/accounts", Some(json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"error": "Missing required fields: vendor_id, name, api_key"})
    );

    let (status, body) =
        request_json(Method::POST, "/api/keys", Some(json!({"name": "test"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["success"], true);
    assert!(body["key"].as_str().unwrap().starts_with("sk-llmux-"));

    let (status, body) =
        request_json(Method::PUT, "/api/settings", Some(json!({"port": "3000"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"success": true}));

    let (status, body) = request_json(Method::POST, "/api/import", Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"success": true, "imported": {"accounts": 0, "aliases": 0, "keys": 0}})
    );

    let (status, body) = request_json(Method::POST, "/api/models/test", Some(json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "No model provided"}));

    let (status, body) = request_json(
        Method::POST,
        "/api/models/test-all",
        Some(json!({"models": []})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"success": true, "message": "Queue started", "total": 0})
    );
}

#[tokio::test]
async fn v1_routes_are_authenticated_placeholders_with_legacy_error_shape() {
    for path in ["/v1/chat/completions", "/v1/messages"] {
        let (status, body) = request_json(Method::POST, path, Some(json!({"model": "demo"}))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            body,
            json!({"error": {"message": "Missing API Key. Gateway is locked.", "type": "authentication_error", "code": "401"}}),
            "{path}"
        );
    }

    let (status, body) = request_json(Method::GET, "/v1/models", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["type"], "authentication_error");

    let (status, body) = request_json(Method::GET, "/v1/models", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "401");
}

#[tokio::test]
async fn account_alias_binding_round_trip_and_cascade() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state);

    // 1. 建账户（skip_validation，不开网络校验）
    let (status, body) = request_json_shared(
        &app,
        Method::POST,
        "/api/accounts",
        Some(json!({"vendor_id": "openai", "name": "BoundAcct", "api_key": "sk-test", "skip_validation": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let account_id = body["id"].as_i64().expect("account id");

    // 2. 用该账户的 vendor_id 建 alias 并绑定（回归：owned_by/外键路径）
    let (status, body) = request_json_shared(
        &app,
        Method::POST,
        "/api/models/aliases",
        Some(json!({"alias": "bounded", "target_model": "gpt-4o", "vendor_id": "openai", "account_ids": [account_id], "preferred_account_id": account_id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "alias+binding should succeed: {body}");

    // 3. 列表能看到绑定与首选
    let (_, aliases) = request_json_shared(&app, Method::GET, "/api/models/aliases", None).await;
    let entry = aliases
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "bounded")
        .expect("alias exists");
    assert_eq!(entry["account_ids"], json!([account_id]));
    assert_eq!(entry["preferred_account_id"], json!(account_id));

    // 4. 删账户 → 绑定 CASCADE 清空，alias 仍保留
    let (status, _) =
        request_json_shared(&app, Method::DELETE, &format!("/api/accounts/{account_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, aliases) = request_json_shared(&app, Method::GET, "/api/models/aliases", None).await;
    let entry = aliases
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "bounded")
        .expect("alias survives account delete");
    assert_eq!(entry["account_ids"], json!([]), "bindings cascaded");
    assert_eq!(entry["preferred_account_id"], Value::Null);
}

#[tokio::test]
async fn unknown_api_routes_return_gateway_not_found_error_without_spa_fallback() {
    let (status, body) = request_json(Method::GET, "/api/does-not-exist", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({"error": {"message": "Not Found", "type": "not_found", "code": "404"}})
    );
}

#[tokio::test]
async fn root_ui_and_non_api_paths_use_spa_fallback() {
    for path in ["/", "/ui", "/ui/", "/dashboard/settings"] {
        let (status, text, content_type) = request_text(Method::GET, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            text.contains("<html") || text.contains("<!doctype html"),
            "{path}: {text}"
        );
        assert!(
            content_type.unwrap_or_default().starts_with("text/html"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn system_tools_returns_expected_structure() {
    let (status, body) = request_json(Method::GET, "/api/system/tools", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_object(), "expected object, got {:?}", body);
    for key in &["vscode", "claude", "gemini", "opencode", "codex"] {
        let val = body.get(key);
        assert!(val.is_some(), "missing key {}", key);
        assert!(val.unwrap().is_boolean(), "key {} should be boolean", key);
    }
    assert_eq!(body.as_object().unwrap().len(), 5);
}

#[tokio::test]
async fn system_claude_settings_returns_valid_structure() {
    let (status, body) = request_json(Method::GET, "/api/system/claude-settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("exists").is_some());
    assert!(body.get("settings").is_some());
}
