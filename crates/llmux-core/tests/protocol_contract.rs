use llmux_core::adapters::Account;
use llmux_core::protocol::{target_protocol, DownstreamMode, Protocol};

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
