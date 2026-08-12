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
    assert_eq!(entry["preferred_account_id"], json!(account_id));
    // 绑定账户数组含该账户，且为首选
    let accounts = entry["accounts"].as_array().expect("accounts array");
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["id"], account_id);
    assert_eq!(accounts[0]["name"], "BoundAcct");
    assert_eq!(accounts[0]["vendor_id"], "openai");
    assert_eq!(accounts[0]["vendor_name"], "OpenAI");
    assert_eq!(accounts[0]["is_preferred"], true);

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
    assert_eq!(entry["accounts"].as_array().unwrap().len(), 0, "bindings cascaded");
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
    assert!(body.get("version").unwrap().is_string(), "version should be string");
    assert_eq!(body.as_object().unwrap().len(), 6);
}

#[tokio::test]
async fn system_claude_settings_returns_valid_structure() {
    let (status, body) = request_json(Method::GET, "/api/system/claude-settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("exists").is_some());
    assert!(body.get("settings").is_some());
}

#[test]
fn effective_openai_base_url_routes_gemini_compat_accounts() {
    use llmux_core::adapters::Account;
    use llmux_server::routes::v1::helpers::effective_openai_base_url;

    let mk = |protocol: &str,
              base_url: Option<String>,
              custom_base_url: bool,
              openai_compatible: i64|
     -> Account {
        Account {
            id: 1,
            name: "a".into(),
            vendor_id: protocol.into(),
            protocol: protocol.into(),
            api_key: "sk".into(),
            base_url,
            anthropic_base_url: None,
            custom_base_url,
            custom_anthropic_base_url: false,
            serves_anthropic: protocol == "anthropic",
            openai_compatible,
            openai_responses: true,
            enabled: 1,
            weight: 1,
        }
    };

    // gemini + openai_compatible + 未自定义 base_url → 官方 OpenAI 兼容端点
    let gem = mk(
        "gemini",
        Some("https://generativelanguage.googleapis.com/v1beta".into()),
        false,
        1,
    );
    assert_eq!(
        effective_openai_base_url(&gem),
        "https://generativelanguage.googleapis.com/v1beta/openai"
    );

    // gemini + openai_compatible + 自定义 base_url → 用自定义
    let gem_custom = mk("gemini", Some("https://proxy.example/v1".into()), true, 1);
    assert_eq!(effective_openai_base_url(&gem_custom), "https://proxy.example/v1");

    // gemini 未开 openai_compatible → 不拼 /openai
    let gem_off = mk(
        "gemini",
        Some("https://generativelanguage.googleapis.com/v1beta".into()),
        false,
        0,
    );
    assert_eq!(
        effective_openai_base_url(&gem_off),
        "https://generativelanguage.googleapis.com/v1beta"
    );

    // openai 协议账户 → 自身 base_url 或默认 openai 端点
    let oai = mk("openai", None, false, 0);
    assert_eq!(effective_openai_base_url(&oai), "https://api.openai.com/v1");
}

#[tokio::test]
async fn gemini_openai_compatible_account_serves_chat_completions() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // mock 上游：记录请求行，按路径返回模型列表或 chat completion
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (path_tx, mut path_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            let path_tx = path_tx.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                let n = match socket.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => return,
                };
                let req_text = String::from_utf8_lossy(&buf[..n]).to_string();
                let request_line = req_text.lines().next().unwrap_or_default().to_string();
                let is_chat_completion = request_line.contains("/chat/completions");
                let _ = path_tx.send(request_line);
                let body = if is_chat_completion {
                    r#"{"id":"cmpl-1","object":"chat.completion","created":0,"model":"gemini","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#
                } else {
                    r#"{"object":"list","data":[]}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state);

    // 建 gemini 账户：openai_compatible=1 + base_url 指向 mock（开启 OpenAI 兼容模式）
    let (status, body) = request_json_shared(
        &app,
        Method::POST,
        "/api/accounts",
        Some(json!({
            "vendor_id": "gemini",
            "name": "GemOpenAI",
            "api_key": "fake-key",
            "base_url": format!("http://{addr}"),
            "openai_compatible": 1,
            "skip_validation": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create gemini compat account: {body}");

    // 建网关 key
    let (status, body) = request_json_shared(
        &app,
        Method::POST,
        "/api/keys",
        Some(json!({"name": "gem-test"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create key: {body}");
    let key = body["key"].as_str().expect("key value").to_string();

    // 带网关 key 调 /v1/chat/completions（gemini 前缀模型）
    let v1_body = json!({
        "model": "gemini-2.0-flash",
        "messages": [{"role": "user", "content": "hi"}]
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/chat/completions")
        .header(header::CONTENT_TYPE, "application/json")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::from(v1_body.to_string()))
        .unwrap();
    let response = llmux_server::test_request(app, request).await;
    let status_code = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let v1_value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    assert_eq!(status_code, StatusCode::OK, "chat completions: {v1_value}");

    // 断言 mock 上游确实收到过 /chat/completions 请求
    let mut hit = false;
    for _ in 0..20 {
        match path_rx.try_recv() {
            Ok(line) if line.contains("/chat/completions") => {
                hit = true;
                break;
            }
            Ok(_) => {}
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
        }
    }
    assert!(hit, "mock upstream should have received a /chat/completions request");
}

#[tokio::test]
async fn alias_list_returns_vendor_aggregation() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state);

    // 建 zai + huoshan 账户
    let (_, body) = request_json_shared(
        &app, Method::POST, "/api/accounts",
        Some(json!({"vendor_id": "zai", "name": "ZaiAgg", "api_key": "sk-zai", "skip_validation": true})),
    ).await;
    let zai_id = body["id"].as_i64().expect("zai id");
    let (_, body) = request_json_shared(
        &app, Method::POST, "/api/accounts",
        Some(json!({"vendor_id": "huoshan", "name": "HsAgg", "api_key": "sk-hs", "skip_validation": true})),
    ).await;
    let hs_id = body["id"].as_i64().expect("hs id");

    // 建 alias 绑两账户，zai 首选
    let (status, _) = request_json_shared(
        &app, Method::POST, "/api/models/aliases",
        Some(json!({"alias": "aggtest", "target_model": "gml-5.2", "account_ids": [zai_id, hs_id], "preferred_account_id": zai_id})),
    ).await;
    assert_eq!(status, StatusCode::OK);

    let (_, aliases) = request_json_shared(&app, Method::GET, "/api/models/aliases", None).await;
    let entry = aliases.as_array().unwrap().iter()
        .find(|a| a["alias"] == "aggtest").expect("alias exists");

    // preferred_account_id 保留（由 is_preferred 推导）
    assert_eq!(entry["preferred_account_id"], zai_id);

    // 绑定账户数组：每个账户带完整信息 + 厂商 + 首选标记
    let accounts = entry["accounts"].as_array().expect("accounts array");
    assert_eq!(accounts.len(), 2);
    let zai_a = accounts.iter().find(|a| a["vendor_id"] == "zai").expect("zai account");
    assert_eq!(zai_a["id"], zai_id);
    assert_eq!(zai_a["name"], "ZaiAgg");
    assert_eq!(zai_a["vendor_name"], "阶跃星辰 StepFun");
    assert_eq!(zai_a["protocol"], "openai");
    assert_eq!(zai_a["is_preferred"], true);
    let hs_a = accounts.iter().find(|a| a["vendor_id"] == "huoshan").expect("hs account");
    assert_eq!(hs_a["id"], hs_id);
    assert_eq!(hs_a["name"], "HsAgg");
    assert_eq!(hs_a["vendor_name"], "火山方舟 Ark");
    assert_eq!(hs_a["is_preferred"], false);

    // 无绑定别名：accounts 空、preferred_account_id null
    request_json_shared(
        &app, Method::POST, "/api/models/aliases",
        Some(json!({"alias": "nobind", "target_model": "claude-3", "account_ids": []})),
    ).await;
    let (_, aliases2) = request_json_shared(&app, Method::GET, "/api/models/aliases", None).await;
    let nobind = aliases2.as_array().unwrap().iter()
        .find(|a| a["alias"] == "nobind").expect("nobind exists");
    assert_eq!(nobind["accounts"].as_array().unwrap().len(), 0);
    assert_eq!(nobind["preferred_account_id"], Value::Null);
}

#[tokio::test]
async fn update_api_key_null_allowed_models_preserves_whitelist() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state);

    // 创建受限 key（白名单 ["gpt-4"]）
    let (status, created) = request_json_shared(
        &app,
        Method::POST,
        "/api/keys",
        Some(json!({"name": "restricted", "allowed_models": ["gpt-4"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let key_id = created["id"].as_i64().expect("key id");

    // PUT 带 allowed_models: null —— 不应清空白名单（受限 key 变不限是权限漏洞）
    let (status, _) = request_json_shared(
        &app,
        Method::PUT,
        &format!("/api/keys/{key_id}"),
        Some(json!({"allowed_models": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 白名单应保留
    let (_, keys) = request_json_shared(&app, Method::GET, "/api/keys", None).await;
    let key = keys
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["id"] == key_id)
        .expect("key exists");
    assert_eq!(key["allowed_models"], json!(["gpt-4"]));
}

#[tokio::test]
async fn update_vendor_partial_body_preserves_unchanged_fields() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state);

    // 只传 name，其余字段应保留（合并更新，而非缺省被重置）
    let (status, _) = request_json_shared(
        &app,
        Method::PUT,
        "/api/vendors/deepseek",
        Some(json!({"name": "DeepSeek-新"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, vendors) = request_json_shared(&app, Method::GET, "/api/vendors", None).await;
    let ds = vendors
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "deepseek")
        .expect("deepseek exists");
    assert_eq!(ds["name"], "DeepSeek-新");
    // 多协议、responses 开关、anthropic 默认端点均不被覆盖
    assert_eq!(ds["protocols"], json!(["openai", "anthropic"]));
    assert_eq!(ds["openai_responses"], json!(true));
    assert_eq!(ds["default_anthropic_url"], "https://api.deepseek.com/anthropic");
}

#[tokio::test]
async fn purge_preserves_app_settings_and_gateway_key() {
    let state = llmux_server::test_state().await;
    let settings = llmux_core::settings::SettingsService::new(state.pool.clone());
    let key = settings.get_or_create_gateway_key().await.expect("gateway key");
    let app = llmux_server::app(state);

    let (status, _) = request_json_shared(&app, Method::POST, "/api/settings/reset", None).await;
    assert_eq!(status, StatusCode::OK);

    // gateway_key 存于 app_settings，清库后不应失效
    let again = settings.get_or_create_gateway_key().await.expect("still readable");
    assert_eq!(key, again);
}

#[tokio::test]
async fn web_session_unknown_provider_gets_own_vendor_not_openai_pool() {
    let state = llmux_server::test_state().await;
    let pool = state.pool.clone();
    let app = llmux_server::app(state);

    let (status, body) = request_json_shared(
        &app,
        Method::POST,
        "/api/auth/web-session",
        Some(json!({"provider": "kimi", "token": "sk-kimi-token"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["success"], true);

    // 账户挂独立 vendor "kimi"，而非 openai 账户池
    let vendor: String = sqlx::query_scalar("SELECT vendor_id FROM accounts WHERE name = 'kimi-web'")
        .fetch_one(&pool)
        .await
        .expect("account exists");
    assert_eq!(vendor, "kimi");

    // 独立 vendor 已自动创建
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vendors WHERE id = 'kimi'")
        .fetch_one(&pool)
        .await
        .expect("vendor exists");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn swagger_ui_injects_request_snippets_config() {
    // config 注入在 swagger-initializer.js（index.html 通过它初始化 Swagger UI）
    let (status, text, _) = request_text(Method::GET, "/swagger/swagger-initializer.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("requestSnippetsEnabled"),
        "swagger-initializer.js 应包含 request snippets 配置，实际: {}",
        &text.chars().take(400).collect::<String>()
    );
}

#[tokio::test]
async fn test_all_queue_writes_health_for_each_model() {
    let state = llmux_server::test_state().await;
    let pool = state.pool.clone();
    let app = llmux_server::app(state);

    // 建 deepseek 账户（目标厂商：内置种子 vendor，有协议/默认 URL）
    let (status, body) = request_json_shared(
        &app,
        Method::POST,
        "/api/accounts",
        Some(json!({"vendor_id": "deepseek", "name": "DSHealth", "api_key": "sk-ds", "skip_validation": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let account_id = body["id"].as_i64().expect("account id");

    // 启动批量拨测：两个模型走同一账户
    let (status, body) = request_json_shared(
        &app,
        Method::POST,
        "/api/models/test-all",
        Some(json!({"models": [
            {"model": "deepseek-chat", "vendorId": "deepseek"},
            {"model": "deepseek-reasoner", "vendorId": "deepseek"}
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["success"], true);

    // 轮询等待队列结束（批量拨测为异步 spawn）
    let mut attempts = 0;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let (_, q) = request_json_shared(&app, Method::GET, "/api/models/test-queue/status", None).await;
        attempts += 1;
        if q["isRunning"] == json!(false) || attempts > 100 {
            break;
        }
    }

    // 健康表应包含传入的两个模型（拨测请求无真实网络，结果为失败也算写入）
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM model_health WHERE account_id = ? AND model IN ('deepseek-chat','deepseek-reasoner')")
            .bind(account_id)
            .fetch_one(&pool)
            .await
            .expect("count health rows");
    assert_eq!(count, 2, "批量拨测应为每个模型写入健康记录，实际 {} 条", count);

    // GET /api/models/health 聚合两个模型
    let (_, health) = request_json_shared(&app, Method::GET, "/api/models/health", None).await;
    let models: Vec<String> = health
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["model"].as_str().map_or(false, |m| m.starts_with("deepseek-")))
        .map(|r| r["model"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(models.len(), 2, "health 接口应返回两个模型，实际 {:?}", models);
}
