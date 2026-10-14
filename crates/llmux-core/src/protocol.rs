use crate::adapters::Account;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Chat,
    Responses,
    Messages,
}

impl Protocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "responses" => Self::Responses,
            "messages" => Self::Messages,
            _ => Self::Chat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownstreamMode {
    Default,
    Chat,
    Responses,
    Messages,
}

impl DownstreamMode {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "chat" => Self::Chat,
            "responses" => Self::Responses,
            "messages" => Self::Messages,
            "auto" | "default" | "" => Self::Default,
            _ => Self::Default,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }
}

pub fn supports(a: &Account, p: Protocol) -> bool {
    endpoint_for(a, p).is_some()
}

pub fn endpoint_for(a: &Account, p: Protocol) -> Option<&str> {
    // 固定地址账户只有 codex：它的后端只认 Responses，地址代码里固定 ——
    // 配错只会指向不存在的容器（codex-api 那次）。zen 是普通账户同形，
    // 地址账户自配（base_url），spec §2 不许钉常量。
    //
    // codex 对 Messages 返回 None 与改动前一致：调用方据此走 OpenAI 转换分支，
    // 而不是把它当 Anthropic 端点去 POST `/v1/messages`
    // （见 `v1/anthropic.rs` 的 `anthropic_base` / `openai_base` 选择）。
    // codex 的后端只认 Responses（`…/backend-api/codex` 拼不出 chat/messages）。
    if a.category.as_str() == "codex" && p != Protocol::Messages {
        return Some(crate::codex_compat::CODEX_BACKEND_BASE);
    }
    // Since 0012 every write keeps base_url = chat_endpoint, so base_url is the
    // chat source of truth; chat_endpoint remains as legacy-compat fallback
    // (e.g. clients explicitly clearing base_url while keeping chat_endpoint).
    match p {
        Protocol::Chat => a
            .base_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| a.chat_endpoint.as_deref().filter(|s| !s.is_empty())),
        Protocol::Responses => a
            .responses_endpoint
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| a.base_url.as_deref().filter(|s| !s.is_empty())),
        // Messages deliberately does NOT fall back to base_url: base_url is an
        // OpenAI-compatible endpoint, and treating it as an Anthropic one would
        // make chat-only accounts POST /v1/messages blindly. Note the route
        // itself usually *does* exist on these hosts (opencode.ai/zen/go/v1/messages
        // answers JSON, not 404) — whether a given model is served there is
        // per-model, so this is about not guessing, not about the route missing.
        Protocol::Messages => a
            .messages_endpoint
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| a.anthropic_base_url.as_deref().filter(|s| !s.is_empty())),
    }
}

pub fn target_protocol(
    ingress: Protocol,
    mode: DownstreamMode,
    account: &Account,
) -> Protocol {
    // codex 原生账户只有 Responses 一种形状：它的后端是 `…/backend-api/codex`，
    // 拼 `/chat/completions` 只会 404。所以别名把 upstream_api 配成 /chat 时
    // **不能照办** —— 否则配置一错，路由就静默拼出一个不存在的路径。
    // 放在最前面：任何 mode（含 forced）都不例外，配置对 codex 不再有影响。
    if crate::adapters::is_codex_native(account) {
        return Protocol::Responses;
    }
    match mode {
        DownstreamMode::Default => {
            if supports(account, ingress) {
                ingress
            } else {
                default_protocol_for(account)
            }
        }
        DownstreamMode::Chat => Protocol::Chat,
        DownstreamMode::Responses => Protocol::Responses,
        DownstreamMode::Messages => Protocol::Messages,
    }
}

pub fn default_protocol_for(a: &Account) -> Protocol {
    match a.default_protocol.as_deref().unwrap_or("chat") {
        "responses" => Protocol::Responses,
        "messages" => Protocol::Messages,
        _ => Protocol::Chat,
    }
}

