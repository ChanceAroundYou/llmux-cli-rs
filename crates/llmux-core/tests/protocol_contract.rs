use llmux_core::adapters::Account;
use llmux_core::protocol::{endpoint_for, supports, target_protocol, DownstreamMode, Protocol};

fn acc(
    chat: Option<&str>,
    resp: Option<&str>,
    msg: Option<&str>,
    def: &str,
) -> Account {
    Account {
        id: 1,
        alias: "x".into(),
        provider_id: "custom".into(),
        api_key: "k".into(),
        base_url: None,
        anthropic_base_url: None,
        chat_endpoint: chat.map(|s| s.to_string()),
        responses_endpoint: resp.map(|s| s.to_string()),
        messages_endpoint: msg.map(|s| s.to_string()),
        default_protocol: Some(def.to_string()),
        balance_provider: String::new(),
        balance_auth: String::new(),
        codex_auth_json: String::new(),
        category: "custom".to_string(),
        is_active: 1,
        weight: 1,
        openai_compatible: 0,
    }
}

#[test]
fn default_mode_passthrough_when_supported() {
    let a = acc(
        Some("https://a/v1"),
        Some("https://a/v1"),
        Some("https://a/v1"),
        "chat",
    );
    assert_eq!(
        target_protocol(Protocol::Chat, DownstreamMode::Default, &a),
        Protocol::Chat
    );
    assert_eq!(
        target_protocol(Protocol::Messages, DownstreamMode::Default, &a),
        Protocol::Messages
    );
}

#[test]
fn default_mode_falls_back_to_default_protocol() {
    let a = acc(Some("https://a/v1"), None, None, "chat");
    assert_eq!(
        target_protocol(Protocol::Responses, DownstreamMode::Default, &a),
        Protocol::Chat
    );
    assert_eq!(
        target_protocol(Protocol::Messages, DownstreamMode::Default, &a),
        Protocol::Chat
    );
}

#[test]
fn base_url_only_account_is_chat_only() {
    // Accounts backed by an OpenAI-compatible base_url (no explicit Anthropic
    // endpoint) must NOT be treated as messages-capable: their /v1/messages
    // route typically doesn't exist. messages ingress converts to chat instead.
    let mut a = acc(Some("https://a/v1"), None, None, "chat");
    a.base_url = Some("https://a/v1".into());
    a.anthropic_base_url = Some(String::new());
    assert_eq!(
        target_protocol(Protocol::Messages, DownstreamMode::Default, &a),
        Protocol::Chat
    );
}

#[test]
fn forced_mode_always_targets_forced() {
    let a = acc(Some("https://a/v1"), Some("https://a/v1"), None, "chat");
    assert_eq!(
        target_protocol(Protocol::Chat, DownstreamMode::Responses, &a),
        Protocol::Responses
    );
    assert_eq!(
        target_protocol(Protocol::Messages, DownstreamMode::Responses, &a),
        Protocol::Responses
    );
}

#[test]
fn codex_account_always_targets_responses_whatever_the_alias_says() {
    // 回归（2026-10-13）：别名把 upstream_api 配成 /chat 时，路由曾照办，
    // 拼出 `…/backend-api/codex/chat/completions` 这种不存在的路径。
    // codex 的地址是固定的，协议没有可选项 —— 任何 mode 都不例外。
    let mut a = acc(Some("http://codex-api:18080/v1"), None, None, "chat");
    a.category = "codex".into();
    for mode in [
        DownstreamMode::Default,
        DownstreamMode::Chat,
        DownstreamMode::Messages,
        DownstreamMode::Responses,
    ] {
        for ingress in [Protocol::Chat, Protocol::Messages, Protocol::Responses] {
            assert_eq!(
                target_protocol(ingress, mode, &a),
                Protocol::Responses,
                "ingress={ingress:?} mode={mode:?}"
            );
        }
    }
}

#[test]
fn codex_account_still_has_no_messages_endpoint() {
    // Messages 对 codex 返回 None 与改动前一致：调用方（`v1/anthropic.rs`）据此
    // 走 OpenAI 转换分支，绝不能把它当 Anthropic 端点去 POST `/v1/messages`
    // （codex 后端没有这个路径）。
    let mut a = acc(None, None, None, "responses");
    a.category = "codex".into();
    assert_eq!(endpoint_for(&a, Protocol::Messages), None);
    assert!(target_protocol(Protocol::Messages, DownstreamMode::Default, &a)
        == Protocol::Responses);
}

#[test]
fn zen_free_account_resolves_all_three_protocols_to_the_fixed_prefix() {
    // sidecar 实现了 `/v1/chat/completions`、`/v1/responses`、`/v1/messages`，
    // 账户字段里写什么都覆盖 —— 尤其是配错成直连 `opencode.ai/zen/v1` 时，
    // 否则 TLS 指纹门直接 403（`deploy/zen-free/` 的注释）。
    let mut a = acc(
        Some("https://opencode.ai/zen/v1"),
        Some("https://opencode.ai/zen/v1"),
        Some("https://opencode.ai/zen/v1"),
        "chat",
    );
    a.category = "zen_free".into();
    assert_eq!(endpoint_for(&a, Protocol::Chat), Some("http://zen-free:6446/v1"));
    assert_eq!(
        endpoint_for(&a, Protocol::Responses),
        Some("http://zen-free:6446/v1")
    );
    assert_eq!(
        endpoint_for(&a, Protocol::Messages),
        Some("http://zen-free:6446/v1")
    );
    assert!(supports(&a, Protocol::Chat)
        && supports(&a, Protocol::Responses)
        && supports(&a, Protocol::Messages));
}

#[test]
fn custom_accounts_still_read_their_own_endpoints() {
    // 固定分支不能漏下来：custom 照旧读自己的字段。
    let a = acc(Some("https://a/v1"), Some("https://b/v1"), None, "responses");
    assert_eq!(endpoint_for(&a, Protocol::Chat), Some("https://a/v1"));
    assert_eq!(endpoint_for(&a, Protocol::Responses), Some("https://b/v1"));
    assert_eq!(endpoint_for(&a, Protocol::Messages), None);
}

#[test]
fn auto_maps_to_default() {
    assert_eq!(DownstreamMode::from_str("auto"), DownstreamMode::Default);
    assert_eq!(
        DownstreamMode::from_str("default"),
        DownstreamMode::Default
    );
}

#[test]
fn endpoint_filter_ignores_empty_string() {
    let a = acc(Some(""), Some("https://a/v1"), None, "responses");
    assert_eq!(
        target_protocol(Protocol::Chat, DownstreamMode::Default, &a),
        Protocol::Responses
    );
}

#[test]
fn protocol_as_str_round_trips() {
    assert_eq!(Protocol::Chat.as_str(), "chat");
    assert_eq!(Protocol::Responses.as_str(), "responses");
    assert_eq!(Protocol::Messages.as_str(), "messages");
    assert_eq!(Protocol::from_str("responses"), Protocol::Responses);
    assert_eq!(Protocol::from_str("messages"), Protocol::Messages);
    assert_eq!(Protocol::from_str("unknown"), Protocol::Chat);
}
