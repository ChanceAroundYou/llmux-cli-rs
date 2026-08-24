use llmux_core::adapters::{
    build_custom_request, build_openai_request, build_passthrough, join_upstream_url, Account,
    ChatMessage, ChatRequest,
};
use llmux_core::protocol::Protocol;
use llmux_core::dispatcher::{
    is_retryable_status, resolve_model_by_prefix, resolve_provider_type, DispatchRouter,
};
use llmux_core::proxy::{
    build_anthropic_passthrough_request, build_anthropic_target_url, extract_anthropic_usage_from_sse,
};
use serde_json::json;

fn account(provider_id: &str) -> Account {
    Account {
        id: 7,
        alias: "primary".to_string(),
        provider_id: provider_id.to_string(),
        api_key: "sk-test".to_string(),
        base_url: None,
        anthropic_base_url: None,
        is_active: 1,
        weight: 10,
        openai_compatible: 0,
        chat_endpoint: None,
        responses_endpoint: None,
        messages_endpoint: None,
        default_protocol: None,
    }
}

fn chat_request() -> ChatRequest {
    ChatRequest {
        model: "claude-3-5-sonnet".to_string(),
        messages: vec![
            ChatMessage {
                role: "user".to_string(),
                content: json!("Hello"),
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning_content: None,
                reasoning_signature: None,
            },
        ],
        stream: Some(false),
        temperature: Some(0.2),
        top_p: None,
        max_tokens: Some(1024),
        stop: Some(json!("END")),
        tools: None,
        tool_choice: None,
        is_test: None,
        anthropic_beta: Some("prompt-caching-2024-07-31".to_string()),
        extra: serde_json::Map::new(),
    }
}

#[test]
fn openai_and_custom_requests_inject_bearer_auth_and_endpoint() {
    let mut acc = account("openai");
    acc.base_url = Some("https://api.openai.example/v1/".to_string());
    let request = chat_request();

    let openai = build_openai_request(&request, &acc);
    assert_eq!(openai.method, "POST");
    assert_eq!(openai.url, "https://api.openai.example/v1/chat/completions");
    assert_eq!(
        openai.headers.get("authorization").unwrap(),
        "Bearer sk-test"
    );
    assert_eq!(openai.body["model"], "claude-3-5-sonnet");

    acc.base_url = Some("https://custom.example/api/".to_string());
    let custom = build_custom_request(&request, &acc);
    assert_eq!(custom.url, "https://custom.example/api/chat/completions");
    assert_eq!(
        custom.headers.get("content-type").unwrap(),
        "application/json"
    );
}

#[test]
fn anthropic_passthrough_patches_model_and_injects_auth_headers() {
    let mut acc = account("anthropic");
    acc.anthropic_base_url = Some("https://anthropic-compatible.example".to_string());

    let outbound = build_anthropic_passthrough_request(
        &json!({"model": "old", "messages": [], "max_tokens": 100, "stream": true}),
        &acc,
        "https://host/api",
        "claude-real",
        Some("beta-header"),
    )
    .unwrap();

    assert_eq!(outbound.url, "https://host/api/v1/messages");
    assert_eq!(outbound.body["model"], "claude-real");
    assert_eq!(outbound.headers.get("x-api-key").unwrap(), "sk-test");
    assert_eq!(
        outbound.headers.get("anthropic-beta").unwrap(),
        "beta-header"
    );
    assert_eq!(
        outbound.headers.get("anthropic-version").unwrap(),
        "2023-06-01"
    );
}

#[test]
fn target_url_construction_handles_v1_suffix() {
    assert_eq!(
        build_anthropic_target_url("https://host/v1/"),
        "https://host/v1/messages"
    );
    assert_eq!(
        build_anthropic_target_url("https://host/api"),
        "https://host/api/v1/messages"
    );
}

#[test]
fn upstream_url_join_deduplicates_adjacent_v1_segments() {
    assert_eq!(
        join_upstream_url("https://host/v1/v1/", "v1/messages"),
        "https://host/v1/messages"
    );
    assert_eq!(
        join_upstream_url("https://host/api/v1", "v1/messages"),
        "https://host/api/v1/messages"
    );
    assert_eq!(
        join_upstream_url("https://host/api/v1?region=cn", "responses"),
        "https://host/api/v1/responses?region=cn"
    );
}

#[test]
fn sse_usage_parsing_merges_max_tokens_across_events() {
    let usage = extract_anthropic_usage_from_sse(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n\
         data: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":5,\"output_tokens\":9,\"cache_read_input_tokens\":2}}\n\n",
    );
    assert_eq!(usage.input_tokens, 5);
    assert_eq!(usage.output_tokens, 9);
    assert_eq!(usage.cache_read_input_tokens, 2);
}

#[test]
fn build_passthrough_selects_endpoint_by_protocol() {
    let acc = Account { id: 1, alias: "x".into(), provider_id: "custom".into(), api_key: "sk".into(),
        base_url: Some("https://old/v1".into()), anthropic_base_url: None,
        chat_endpoint: Some("https://a.example/v1".into()), responses_endpoint: Some("https://a.example/v1".into()),
        messages_endpoint: Some("https://a.example/v1".into()), default_protocol: Some("chat".into()),
        is_active: 1, weight: 1, openai_compatible: 0 };
    let req = llmux_core::adapters::build_passthrough(&acc, Protocol::Messages, &json!({"model":"m"}));
    assert!(req.url.ends_with("/v1/messages"));
    let req2 = llmux_core::adapters::build_passthrough(&acc, Protocol::Chat, &json!({"model":"m"}));
    assert!(req2.url.ends_with("/chat/completions"));
}

#[test]
fn dispatcher_helpers_resolve_and_order_failover_attempts() {
    assert_eq!(
        resolve_model_by_prefix("claude-3-haiku").provider_id,
        "anthropic"
    );
    assert_eq!(
        resolve_model_by_prefix("gemini-1.5-pro").provider_id,
        "gemini"
    );
    assert_eq!(resolve_model_by_prefix("gpt-4o").provider_id, "openai");
    assert_eq!(
        resolve_provider_type(Some("custom-anthropic"), "anything"),
        "anthropic"
    );
    assert_eq!(resolve_provider_type(Some("poe"), "poe"), "custom");
    assert!(is_retryable_status(429));
    assert!(is_retryable_status(401));
    assert!(!is_retryable_status(500));

    let accounts = vec![
        Account {
            id: 1,
            alias: "a".into(),
            weight: 1,
            ..account("openai")
        },
        Account {
            id: 2,
            alias: "b".into(),
            weight: 1,
            ..account("openai")
        },
        Account {
            id: 3,
            alias: "c".into(),
            weight: 1,
            ..account("openai")
        },
    ];
    let mut router = DispatchRouter::default();
    let (ordered, meta) = router.select("test_key", &accounts, 1);
    assert!(!meta.is_probe);
    assert_eq!(meta.preferred_id, 1);
    // Preferred account (id=1) should be first
    assert_eq!(ordered[0].id, 1);
}
